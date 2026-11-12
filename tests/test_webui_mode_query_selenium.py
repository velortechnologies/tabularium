"""Document display hints survive deep links and reload without serializing drafts."""
from urllib.parse import quote, parse_qs, urlsplit
import pytest
from selenium.webdriver.common.by import By
from selenium.webdriver.support.ui import WebDriverWait
from tests.helpers import _base, _slug
from tests.test_webui_markdown_media_selenium import rpc

pytestmark = pytest.mark.webui

@pytest.mark.parametrize('hint,selector', [
    ('meta', '[data-testid="preview-meta"][aria-pressed="true"]'),
    ('raw', '[data-testid="preview-raw-toggle"]'),
    ('chat', '[data-testid="preview-chat"]'),
    ('edit', '[data-testid="preview-cancel-edit"]'),
])
def test_document_mode_query(selenium_driver, hint, selector):
    base = _base()
    path = '/mode_' + _slug() + '.md'
    rpc(base, 'put_document', path=path, content='# Mode preview\n\nOriginal text')
    d = selenium_driver
    d.get(base + '/entries?open=' + quote(path, safe='') + '&full=1&' + hint + '=1&custom=keep')
    wait = WebDriverWait(d, 20)
    for refresh in [False, True]:
        if refresh: d.refresh()
        button = wait.until(lambda d: d.find_element(By.CSS_SELECTOR, selector))
        full = wait.until(lambda d: d.find_element(By.CSS_SELECTOR, '[data-testid="preview-full-toggle"]'))
        assert full.text == 'Dock'
        if hint == 'raw': assert button.text == 'MD'
        if hint == 'chat': assert button.text == 'Preview'
    if hint == 'edit':
        editor = d.find_element(By.CSS_SELECTOR, 'textarea')
        editor.send_keys(' PRIVATE_UNSAVED_DRAFT')
        d.find_element(By.CSS_SELECTOR, '[data-testid="preview-full-toggle"]').click()
        assert 'PRIVATE_UNSAVED_DRAFT' in editor.get_attribute('value')
        assert 'PRIVATE_UNSAVED_DRAFT' not in d.current_url
        d.find_element(By.CSS_SELECTOR, '[data-testid="preview-cancel-edit"]').click()
        wait.until(lambda d: 'edit' not in parse_qs(urlsplit(d.current_url).query))
    else:
        d.find_element(By.CSS_SELECTOR, '[data-testid="preview-full-toggle"]').click()
        wait.until(lambda d: 'full' not in parse_qs(urlsplit(d.current_url).query))
    assert parse_qs(urlsplit(d.current_url).query)['custom'] == ['keep']
