"""Named snapshots: real RPC/REST/CLI lifecycle and binary persistence."""
import os
import subprocess
import requests
import pytest
from tests.helpers import _base, _rpc, _slug, _tb_bin, mkdir, put_doc

def rpc(method, **params):
    response = _rpc(method, params)
    assert "error" not in response, response
    return response["result"]

def cli(*args, success=True, env=None):
    result = subprocess.run([_tb_bin(), "-u", _base(), *args], capture_output=True, text=True, env=env, timeout=30)
    assert (result.returncode == 0) == success, result.stderr + result.stdout
    return result

@pytest.fixture
def folder():
    root = "snap_" + _slug()
    mkdir(_base(), root)
    yield "/" + root
    rpc("delete_directory", path="/"+root, recursive=True)

def test_snapshot_lifecycle_and_search(folder):
    path=folder+"/note.md"
    rpc("put_document",path=path,content="ancientneedle")
    original=rpc("get_document",path=path)
    rpc("set_document_metadata",path=path,metadata={"stage":"old"})
    cli("snapshot",path+"@first")
    first=rpc("get_document",path=path+"@first")
    assert first["id"]!=original["id"]
    rpc("put_document",path=path,content="modernneedle",force=True)
    rpc("set_document_metadata",path=path,metadata={"stage":"new"})
    cli("snapshot",path+"@second")
    assert rpc("get_document",path=path+"@first")["content"]=="ancientneedle"
    assert rpc("search",query="ancientneedle",path=folder,kind="lexical")==[]
    hits=rpc("search",query="ancientneedle",path=folder,kind="lexical",include_snapshots=True)
    assert [h["path"] for h in hits]==[path+"@first"]
    cli("rollback",path+"@first")
    live=rpc("get_document",path=path)
    assert live["id"]==original["id"] and live["content"]=="ancientneedle"
    assert live["metadata"]=={"stage":"old"}
    assert live["revision"]!=first["revision"]
    assert rpc("get_document",path=path+"@second")["content"]=="modernneedle"
    cli("mv",path+"@first",path+"@renamed")
    assert rpc("get_document",path=path+"@renamed")["id"]==first["id"]
    assert path+"@renamed" in cli("ls","--snapshot",path).stdout
    cli("cp",path+"@second",folder+"/copy.md")
    copy=rpc("get_document",path=folder+"/copy.md")
    assert copy["content"]=="modernneedle" and copy["id"] not in (live["id"],first["id"])
    cli("rm",path,success=False)
    cli("rm",path+"@renamed")
    rpc("reindex",kind="lexical")
    assert all(h["path"]!=path+"@renamed" for h in rpc("search",query="ancientneedle",kind="lexical",include_snapshots=True))
    cli("rm","-r",path)
    assert "error" in _rpc("get_document",{"path":path+"@second"})

@pytest.mark.parametrize("name",["","bad/name","bad@name","bad name","киця"])
def test_bad_snapshot_names(folder,name):
    path=folder+"/doc"
    rpc("put_document",path=path,content="content")
    assert "error" in _rpc("create_snapshot",{"path":path,"name":name})
    assert rpc("list_snapshots",path=path)==[]

def test_recursive_atomicity_copy_and_rename(folder):
    rpc("create_directory",path=folder+"/sub")
    for suffix in ["/a","/sub/b"]:
        rpc("put_document",path=folder+suffix,content=suffix)
    rpc("create_snapshot",path=folder+"/sub/b",name="conflict")
    assert "error" in _rpc("create_snapshot",{"path":folder,"name":"conflict","recursive":True})
    assert rpc("list_snapshots",path=folder+"/a")==[]
    cli("snapshot","-r",folder+"@all")
    rpc("put_document",path=folder+"/new",content="no snapshot")
    cli("cp","-r",folder+"@all",folder+"_copy")
    try:
        assert rpc("get_document",path=folder+"_copy/a")["content"]=="/a"
        assert not rpc("exists",path=folder+"_copy/new")
        cli("mv","-r",folder+"@all",folder+"@renamed")
        assert len(rpc("list_snapshots",path=folder,name="renamed",recursive=True))==2
        cli("rollback","-r",folder+"@renamed")
        assert rpc("get_document",path=folder+"/new")["content"]=="no snapshot"
        cli("rm","-r",folder+"@renamed")
        assert rpc("list_snapshots",path=folder,name="renamed",recursive=True)==[]
    finally:
        rpc("delete_directory",path=folder+"_copy",recursive=True)

def test_binary_snapshots_and_rest(folder,tmp_path):
    path=folder+"/blob.bin"
    before=b"\x00\xff\x80old"
    after=b"\x00\xfe\x81new"
    url=_base()+"/api/content"+path
    response=requests.put(url,data=before,headers={"Content-Type":"application/octet-stream"},timeout=10)
    response.raise_for_status()
    response=requests.post(_base()+"/api/snapshots/create",json={"path":path,"name":"original"},timeout=10)
    response.raise_for_status()
    requests.put(url,data=after,headers={"Content-Type":"application/octet-stream"},timeout=10).raise_for_status()
    assert requests.get(url+"@original",timeout=10).content==before
    marker=tmp_path/"editor-ran"
    editor=tmp_path/"editor"
    editor.write_text("#!/bin/sh\ntouch "+str(marker)+"\n")
    editor.chmod(0o700)
    cli("edit",path,success=False,env=dict(os.environ,VISUAL=str(editor)))
    assert not marker.exists()
    cli("cp",path+"@original",folder+"/restored.bin")
    assert requests.get(_base()+"/api/content"+folder+"/restored.bin",timeout=10).content==before
    cli("rollback",path+"@original")
    assert requests.get(url,timeout=10).content==before
    requests.delete(_base()+"/api/doc"+path,params={"recursive":"true"},timeout=10).raise_for_status()
    assert requests.get(url+"@original",timeout=10).status_code==404

def test_immutable_snapshots(folder):
    path=folder+"/doc"
    rpc("put_document",path=path,content="immutable")
    rpc("create_snapshot",path=path,name="frozen")
    for method,params in [
        ("put_document",{"content":"changed","force":True}),
        ("append_document",{"content":"changed","force":True}),
        ("set_document_metadata",{"metadata":{"bad":True}}),
        ("describe",{"description":"changed"}),
        ("move_document",{"new_path":folder+"/other@new"}),
    ]:
        assert "error" in _rpc(method,dict(path=path+"@frozen",**params))
    assert rpc("get_document",path=path+"@frozen")["content"]=="immutable"


def test_rename_conflict_is_atomic_and_source_move_follows(folder):
    for name in ("a","b"):
        rpc("put_document",path=folder+"/"+name,content="saved "+name)
    cli("snapshot","-r",folder+"@old")
    rpc("create_snapshot",path=folder+"/b",name="taken")
    cli("mv","-r",folder+"@old",folder+"@taken",success=False)
    assert len(rpc("list_snapshots",path=folder,name="old",recursive=True))==2
    cli("mv",folder+"/a",folder+"/moved")
    assert rpc("get_document",path=folder+"/moved@old")["content"]=="saved a"
    cli("cp",folder+"/moved@old",folder+"/b",success=False)
    assert rpc("get_document",path=folder+"/b")["content"]=="saved b"
    cli("snapshot",folder+"@no-recursion",success=False)
    cli("rollback",folder+"@old",success=False)
    cli("cp",folder+"@old",folder+"_not_created",success=False)
    assert not rpc("exists",path=folder+"_not_created")
    cli("rm",folder+"@old",success=False)
    assert rpc("list_snapshots",path=folder+"/moved",name="missing")==[]
    cli("rollback",folder+"/moved@missing",success=False)

def test_snapshot_text_binary_transition_and_export(folder,tmp_path):
    path=folder+"/document"
    rpc("put_document",path=path,content="")
    cli("snapshot",path+"@empty")
    data=b"\x00\xffbinary"
    requests.put(_base()+"/api/content"+path,data=data,headers={"Content-Type":"application/octet-stream"},timeout=10).raise_for_status()
    cli("snapshot",path+"@binary")
    requests.put(_base()+"/api/content"+path,data=b"new text",headers={"Content-Type":"text/plain"},timeout=10).raise_for_status()
    assert rpc("get_document",path=path+"@empty")["content"]==""
    cli("rollback",path+"@binary")
    assert requests.get(_base()+"/api/content"+path,timeout=10).content==data
    out=tmp_path/"export";out.mkdir()
    cli("export",path+"@binary","-d",str(out))
    assert (out/"document").read_bytes()==data
    cli("rollback",path+"@empty")
    assert rpc("get_document",path=path)["content"]==""
    assert requests.get(_base()+"/api/content"+path+"@binary",timeout=10).content==data
