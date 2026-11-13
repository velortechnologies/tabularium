"""Snapshot preview selection survives refresh and remains read-only."""
import pytest
from urllib.parse import quote
from selenium.webdriver.common.by import By
from selenium.webdriver.support.ui import Select, WebDriverWait
from tests.helpers import _base,_slug,_rpc
pytestmark=pytest.mark.webui

def test_snapshot_selector_refresh(selenium_driver):
    def rpc(method,**params):
        r=_rpc(method,params); assert "error" not in r,r
        return r["result"]
    path="/snapshot_ui_"+_slug()+".md"
    token="snapshotneedle"+_slug()
    rpc("put_document",path=path,content="old snapshot body "+token)
    rpc("create_snapshot",path=path,name="old")
    rpc("put_document",path=path,content="current live body",force=True)
    d=selenium_driver; wait=WebDriverWait(d,20)
    try:
        d.get(_base()+"/entries?open="+quote(path,safe=""))
        dropdown=wait.until(lambda d:d.find_element(By.CSS_SELECTOR,'[data-testid="preview-snapshot"]'))
        wait.until(lambda d:len(Select(dropdown).options)==2 and dropdown.is_enabled())
        assert Select(dropdown).first_selected_option.get_attribute("value")==""
        Select(dropdown).select_by_value("old")
        wait.until(lambda d:"snapshot=old" in d.current_url)
        wait.until(lambda d:"old snapshot body" in d.find_element(By.CSS_SELECTOR,'[data-testid="preview-pane"]').text)
        assert not d.find_element(By.CSS_SELECTOR,'[data-testid="preview-edit"]').is_enabled()
        d.refresh()
        wait.until(lambda d:Select(d.find_element(By.CSS_SELECTOR,'[data-testid="preview-snapshot"]')).first_selected_option.get_attribute("value")=="old")
        Select(d.find_element(By.CSS_SELECTOR,'[data-testid="preview-snapshot"]')).select_by_value("")
        wait.until(lambda d:"snapshot=" not in d.current_url)
        wait.until(lambda d:"current live body" in d.find_element(By.CSS_SELECTOR,'[data-testid="preview-pane"]').text)
        checkbox=d.find_element(By.CSS_SELECTOR,'[data-testid="search-include-snapshots"]')
        assert checkbox.get_attribute("type")=="checkbox" and not checkbox.is_selected()
        Select(d.find_element(By.CSS_SELECTOR,'[data-testid="search-kind"]')).select_by_value("lexical")
        d.find_element(By.CSS_SELECTOR,'[data-testid="search-input"]').send_keys(token)
        d.find_element(By.CSS_SELECTOR,'[data-testid="search-submit"]').click()
        wait.until(lambda d:d.find_element(By.CSS_SELECTOR,'[data-testid="search-mode"]'))
        checkbox.click()
        row=wait.until(lambda d:d.find_element(By.CSS_SELECTOR,'[data-testid="search-result-row"]'))
        assert "@old" in row.text
        row.click()
        wait.until(lambda d:"snapshot=old" in d.current_url)
        assert not d.find_element(By.CSS_SELECTOR,'[data-testid="preview-edit"]').is_enabled()
        checkbox.click()
        wait.until(lambda d:not d.find_elements(By.CSS_SELECTOR,'[data-testid="search-result-row"]'))
    finally:
        rpc("delete_document",path=path,recursive=True)
