"""Embedded stored-media paths, MIME dispatch and real browser playback."""
import io
import struct
import wave
from pathlib import Path
from urllib.parse import quote
import pytest
import requests
from selenium.webdriver.common.by import By
from selenium.webdriver.support.ui import WebDriverWait
from tests.helpers import _base, _slug
from tests.test_webui_binary_selenium import png

pytestmark = pytest.mark.webui


def rpc(base, method, **params):
    response = requests.post(base + '/rpc', json={'jsonrpc': '2.0', 'id': 1, 'method': method, 'params': params}, timeout=20)
    response.raise_for_status()
    value = response.json()
    assert 'error' not in value, value
    return value['result']


@pytest.mark.parametrize('style', ['absolute', 'bare', 'dot', 'parent'])
def test_markdown_media_paths_and_playback(selenium_driver, style):
    base = _base()
    root = '/embed_' + _slug()
    rpc(base, 'create_directory', path=root + '/notes', parents=True)
    wav = io.BytesIO()
    with wave.open(wav, 'wb') as f:
        f.setnchannels(1); f.setsampwidth(2); f.setframerate(8000)
        f.writeframes(struct.pack('<h', 400) * 16000)
    fixtures = [('picture.bin', 'image/png', png(16, 16)),
                ('sound.bin', 'audio/wav', wav.getvalue()),
                ('movie.bin', 'video/webm', (Path(__file__).parent / 'fixtures/embed-video.webm').read_bytes())]
    media_dir = root if style == 'parent' else root + '/notes'
    for name, mime, data in fixtures:
        requests.put(base + '/api/content' + media_dir + '/' + name, data=data, headers={'Content-Type': mime}, timeout=20).raise_for_status()
    prefix = {'absolute': media_dir + '/', 'bare': '', 'dot': './', 'parent': '../'}[style]
    body = '# Embedded media\n\nText before.\n\n![](' + prefix + 'picture.bin)\n\n![Audio caption](' + prefix + 'sound.bin)\n\n![Video caption](' + prefix + 'movie.bin)\n\n![](./missing.png)\n\n[Ordinary link](' + prefix + 'picture.bin)\n\nText after.'
    doc = root + '/notes/readme.md'
    rpc(base, 'put_document', path=doc, content=body)
    d = selenium_driver
    d.get(base + '/entries?open=' + quote(doc, safe=''))
    wait = WebDriverWait(d, 20)
    embeds = wait.until(lambda d: d.find_elements(By.CSS_SELECTOR, '[data-testid="markdown-media"]') if len(d.find_elements(By.CSS_SELECTOR, '[data-testid="markdown-media"]')) == 3 else False)
    image = wait.until(lambda d: next((x for x in d.find_elements(By.CSS_SELECTOR, '[data-testid="markdown-media"] img') if d.execute_script('return arguments[0].complete && arguments[0].naturalWidth>0', x)), False))
    assert image.get_attribute('alt') == ''
    assert len(image.find_element(By.XPATH, '../..').find_elements(By.XPATH, './span')) == 0  # empty caption has no row
    wait.until(lambda d: d.find_elements(By.CSS_SELECTOR, '[data-testid="media-unavailable"]'))
    for tag in ['audio', 'video']:
        element = wait.until(lambda d: next((x for x in d.find_elements(By.CSS_SELECTOR, '[data-testid="markdown-media"] ' + tag) if d.execute_script('return arguments[0].readyState>=1', x)), False))
        assert d.execute_script('return arguments[0].paused && arguments[0].controls && !arguments[0].autoplay', element)
        element.click()
        assert d.execute_async_script('const done=arguments[1];arguments[0].play().then(()=>done(true)).catch(()=>done(false));', element)
        wait.until(lambda d: d.execute_script('return arguments[0].currentTime>0.05', element))
        d.execute_script('arguments[0].pause()', element)
    assert len(embeds) == 3
    assert d.find_element(By.LINK_TEXT, 'Ordinary link').get_attribute('href').find('/entries') != -1
    assert 'Text after.' in d.find_element(By.TAG_NAME, 'body').text
