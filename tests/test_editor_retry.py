"""Metadata editor retry: real PTY, latest buffer and cancellation."""
import os
import pty
import select
import signal
import time
import pytest
from tests.test_snapshots import rpc, folder
from tests.helpers import _base, _tb_bin

def wait_text(fd, expected, transcript, timeout=20):
    end=time.monotonic()+timeout
    while expected not in transcript:
        assert time.monotonic()<end, transcript.decode(errors="replace")
        if select.select([fd],[],[],0.2)[0]:
            try: data=os.read(fd,4096)
            except OSError: data=b""
            assert data, transcript.decode(errors="replace")
            transcript+=data
    return transcript

@pytest.mark.parametrize("cancel",[False,True])
def test_metadata_editor_retry(folder,tmp_path,cancel):
    path=folder+"/doc"
    rpc("put_document",path=path,content="body unchanged")
    editor=tmp_path/"editor.py"
    calls=tmp_path/"calls"
    editor.write_text("""#!/usr/bin/env python3
import pathlib,sys
log=pathlib.Path(sys.argv[1]); buffer=pathlib.Path(sys.argv[2])
previous=log.read_text().splitlines() if log.exists() else []
with log.open("a") as f: f.write(str(buffer)+"\\n")
if not previous: buffer.write_text("{invalid")
else:
    assert str(buffer)==previous[0]
    assert buffer.read_text()=="{invalid"
    buffer.write_text('{"fixed":true}')
""")
    editor.chmod(0o700)
    pid,fd=pty.fork()
    if pid==0:
        env=dict(os.environ,VISUAL=str(editor)+" "+str(calls))
        os.execve(_tb_bin(),[_tb_bin(),"-u",_base(),"edit","--metadata",path],env)
    try:
        transcript=wait_text(fd,b"Press any key to edit again, Ctrl-C to exit",b"")
        os.write(fd,b"\x03" if cancel else b"x")
        end=time.monotonic()+20
        while True:
            result,status=os.waitpid(pid,os.WNOHANG)
            if result: break
            assert time.monotonic()<end, transcript.decode(errors="replace")
            if select.select([fd],[],[],0.1)[0]:
                try: transcript+=os.read(fd,4096)
                except OSError: pass
        assert os.waitstatus_to_exitcode(status)==(130 if cancel else 0),transcript
        value=rpc("get_document",path=path)
        assert value["content"]=="body unchanged"
        assert value["metadata"]==({} if cancel else {"fixed":True})
        paths=calls.read_text().splitlines()
        assert len(paths)==(1 if cancel else 2)
        if cancel:
            from pathlib import Path
            recovery=Path(paths[0])
            assert recovery.read_text()=="{invalid"
            recovery.unlink()
    finally:
        try: os.kill(pid,signal.SIGKILL)
        except ProcessLookupError: pass
        os.close(fd)


@pytest.mark.parametrize("payload,exit_code",[
    ("{broken",0),("[]",0),('{"large":"'+"x"*65536+'"}',0),('{"valid":true}',7),
])
def test_noninteractive_metadata_failure_preserves_buffer(folder,tmp_path,payload,exit_code):
    import subprocess
    from pathlib import Path
    path=folder+"/metadata"
    rpc("put_document",path=path,content="unchanged")
    before=rpc("get_document",path=path)
    payload_file=tmp_path/"payload";payload_file.write_text(payload)
    buffer_path=tmp_path/"buffer"
    editor=tmp_path/"editor.py"
    editor.write_text("#!/usr/bin/env python3\nimport pathlib,sys\np=pathlib.Path(sys.argv[-1])\np.write_text(pathlib.Path("+repr(str(payload_file))+").read_text())\npathlib.Path("+repr(str(buffer_path))+").write_text(str(p))\nsys.exit("+str(exit_code)+")\n")
    editor.chmod(0o700)
    result=subprocess.run([_tb_bin(),"-u",_base(),"edit","--metadata",path],
        env=dict(os.environ,VISUAL=str(editor)),capture_output=True,text=True,start_new_session=True,timeout=15)
    assert result.returncode!=0
    assert "\x1b[" not in result.stderr
    recovery=Path(buffer_path.read_text())
    assert str(recovery) in result.stderr
    assert recovery.read_text()==payload
    recovery.unlink()
    after=rpc("get_document",path=path)
    assert after["revision"]==before["revision"]
    assert after["metadata"]==before["metadata"]

@pytest.mark.parametrize("binary",[False,True])
@pytest.mark.parametrize("alias",[False,True])
def test_metadata_success_noop_and_snapshot_rejection(folder,tmp_path,binary,alias):
    import subprocess,base64
    path=folder+"/doc"
    data=b"\x00\xffbody" if binary else b"text body"
    if binary: rpc("put_binary",path=path,content_base64=base64.b64encode(data).decode(),mime_type="application/octet-stream")
    else: rpc("put_document",path=path,content=data.decode())
    editor=tmp_path/"editor.py"
    marker=tmp_path/"called"
    editor.write_text("#!/usr/bin/env python3\nimport pathlib,sys\npathlib.Path(sys.argv[-1]).write_text('{\"key\":\"value\"}')\npathlib.Path("+repr(str(marker))+").touch()\n")
    editor.chmod(0o700)
    def run(target):
        args=["meta",target,"--edit"] if alias else ["edit","--metadata",target]
        return subprocess.run([_tb_bin(),"-u",_base(),*args],env=dict(os.environ,VISUAL=str(editor)),capture_output=True,text=True,start_new_session=True,timeout=15)
    result=run(path);assert result.returncode==0,result.stderr
    before=rpc("get_document",path=path)
    assert before["metadata"]=={"key":"value"}
    assert run(path).returncode==0
    assert rpc("get_document",path=path)["revision"]==before["revision"]
    if binary: assert base64.b64decode(rpc("get_binary",path=path)["content_base64"])==data
    else: assert before["content"]==data.decode()
    rpc("create_snapshot",path=path,name="immutable")
    marker.unlink()
    assert run(path+"@immutable").returncode!=0
    assert not marker.exists()


def test_metadata_revision_conflict_keeps_server_and_buffer(folder,tmp_path):
    import subprocess,json
    from pathlib import Path
    path=folder+"/conflict"
    rpc("put_document",path=path,content="body")
    editor=tmp_path/"editor.py"
    buffer_path=tmp_path/"buffer"
    request=json.dumps({"jsonrpc":"2.0","id":1,"method":"set_document_metadata","params":{"path":path,"metadata":{"concurrent":True}}})
    editor.write_text("#!/usr/bin/env python3\nimport pathlib,sys,urllib.request\nb=pathlib.Path(sys.argv[-1]); b.write_text('{\"mine\":true}')\npathlib.Path("+repr(str(buffer_path))+").write_text(str(b))\nr=urllib.request.Request("+repr(_base()+"/rpc")+",data="+repr(request.encode())+",headers={'Content-Type':'application/json'})\nwith urllib.request.urlopen(r) as response: response.read()\n")
    editor.chmod(0o700)
    result=subprocess.run([_tb_bin(),"-u",_base(),"edit","--metadata",path],
        env=dict(os.environ,VISUAL=str(editor)),capture_output=True,text=True,start_new_session=True,timeout=15)
    assert result.returncode!=0
    assert rpc("get_document",path=path)["metadata"]=={"concurrent":True}
    recovery=Path(buffer_path.read_text())
    assert recovery.read_text()=='{"mine":true}'
    assert str(recovery) in result.stderr
    recovery.unlink()
