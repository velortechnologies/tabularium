"""Binary REST/RPC and CLI import/export contracts; included by the normal CI suite."""
import base64
import hashlib
import json
import os
import subprocess
import requests
from tests.helpers import _base, _rpc, _slug, _tb_bin, mkdir


def test_binary_import_export_and_replacement(tmp_path):
    base = _base()
    root = 'binary_' + _slug()
    mkdir(base, root)
    source = tmp_path / 'source'
    source.mkdir()
    payload = bytes(range(256)) * 1000
    (source / 'payload.bin').write_bytes(payload)
    (source / 'empty.bin').write_bytes(b'')
    (source / 'payload.bin.metadata.json').write_text(json.dumps({'tag':'binary-'+root}))
    def cli(*args):
        result = subprocess.run([_tb_bin(), '-u', base, *args],capture_output=True,text=True)
        assert result.returncode == 0, result.stdout + result.stderr
    cli('import','/'+root,str(source))
    path = '/'+root+'/payload.bin'
    meta = _rpc('get_document',{'path':path})['result']
    assert meta['content'] is None
    assert meta['content_kind'] == 'binary_attachment'
    assert meta['sha256'] == hashlib.sha256(payload).hexdigest()
    assert requests.get(base+'/api/content'+path,timeout=10).content == payload
    out = tmp_path / 'export'
    out.mkdir()
    cli('export','/'+root,'-d',str(out))
    assert (out/'payload.bin').read_bytes() == payload
    assert (out/'empty.bin').read_bytes() == b''
    assert json.loads((out/'payload.bin.metadata.json').read_text()) == {'tag':'binary-'+root}
    encoded = base64.b64encode(b'\x00replacement\xff').decode()
    result = _rpc('put_binary',{'path':path,'content_base64':encoded,'mime_type':'application/octet-stream','only_if_revision':meta['revision']})
    assert 'result' in result, result
    assert _rpc('get_binary',{'path':path})['result']['content_base64'] == encoded
    response = requests.put(base+'/api/content'+path,data=b'text replacement',headers={'Content-Type':'text/plain'},timeout=10)
    response.raise_for_status()
    assert _rpc('get_document',{'path':path})['result']['content'] == 'text replacement'
