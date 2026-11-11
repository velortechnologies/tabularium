"""Binary media preview contract on the designated CI host."""
import struct
import zlib
from urllib.parse import quote
import pytest
import requests
from selenium.webdriver.common.by import By
from selenium.webdriver.support.ui import WebDriverWait
from tests.helpers import _base, _slug

pytestmark = pytest.mark.webui

def png(w, h):
    def chunk(kind, data):
        return struct.pack('!I',len(data))+kind+data+struct.pack('!I',zlib.crc32(kind+data))
    return b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('!2I5B',w,h,8,2,0,0,0))+chunk(b'IDAT',zlib.compress((b'\0'+b'\x80\x40\x20'*w)*h))+chunk(b'IEND',b'')

@pytest.mark.parametrize('width,height',[(10,1000),(1000,10)])
def test_binary_preview_bounded(selenium_driver,width,height):
    base=_base(); path='/preview_'+_slug()+'.png'
    r=requests.put(base+'/api/content'+path,data=png(width,height),headers={'Content-Type':'image/png'},timeout=20);r.raise_for_status()
    d=selenium_driver
    d.get(base+'/entries?open='+quote(path,safe=''))
    img=WebDriverWait(d,20).until(lambda d: next((i for i in d.find_elements(By.CSS_SELECTOR,'img') if i.get_attribute('alt')==path and d.execute_script('return arguments[0].complete && arguments[0].naturalWidth > 0',i)),False))
    assert d.execute_script('const a=arguments[0].getBoundingClientRect(),b=arguments[0].parentElement.getBoundingClientRect();return a.width<=b.width+1 && a.height<=b.height+1',img)


def test_audio_preview_playback_and_seek(selenium_driver):
    import io
    import math
    import wave
    body = io.BytesIO()
    with wave.open(body, 'wb') as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(8000)
        wav.writeframes(b''.join(struct.pack('<h', int(1000 * math.sin(i * 2 * math.pi * 440 / 8000))) for i in range(16000)))
    base = _base()
    path = '/audio_' + _slug() + '.wav'
    response = requests.put(base + '/api/content' + path, data=body.getvalue(), headers={'Content-Type': 'audio/wav'}, timeout=20)
    response.raise_for_status()
    response = requests.get(base + '/api/content' + path, headers={'Range': 'bytes=0-99'}, timeout=20)
    assert response.status_code == 206
    assert response.headers['Content-Disposition'].startswith('inline')
    d = selenium_driver
    d.get(base + '/entries?open=' + quote(path, safe=''))
    wait = WebDriverWait(d, 20)
    media = wait.until(lambda d: next((e for e in d.find_elements(By.TAG_NAME, 'audio') if d.execute_script('return arguments[0].readyState>=1', e)), False))
    assert d.execute_script('return arguments[0].paused && !arguments[0].autoplay && arguments[0].controls', media)
    media.click()  # Establish a user gesture before programmatic play.
    assert d.execute_async_script('const done=arguments[1];arguments[0].play().then(()=>done(true)).catch(()=>done(false));', media)
    wait.until(lambda d: d.execute_script('return arguments[0].currentTime>0.1', media))
    d.execute_script('arguments[0].pause();arguments[0].currentTime=1', media)
    wait.until(lambda d: d.execute_script('return !arguments[0].seeking && Math.abs(arguments[0].currentTime-1)<0.1', media))
