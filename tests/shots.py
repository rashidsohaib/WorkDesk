import json, os, subprocess, sys, time, urllib.request
BIN = sys.argv[1]; OUT = sys.argv[2]; HOME = '/tmp/wdshots'; PORT_IMAP = 14144
subprocess.run(['rm', '-rf', HOME]); os.makedirs(HOME + '/data'); os.makedirs(HOME + '/cfg'); os.makedirs(OUT, exist_ok=True)
imap = subprocess.Popen([sys.executable, os.path.dirname(os.path.abspath(__file__)) + '/fake_imap.py', str(PORT_IMAP), HOME + '/imap.log'], env=dict(os.environ, WD_DEMO='1'))
time.sleep(0.8)
env = dict(os.environ, HOME=HOME, XDG_DATA_HOME=HOME + '/data', XDG_CONFIG_HOME=HOME + '/cfg', WORKDESK_SHELL='electron', MAILDESK_IMAP_ADDR=f'127.0.0.1:{PORT_IMAP}')
app = subprocess.Popen([BIN], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
ready = json.loads(app.stdout.readline().decode().split(' ', 1)[1]); P, T = ready['port'], ready['token']
def rpc(cmd, **a):
    r = urllib.request.Request(f'http://127.0.0.1:{P}/rpc/{cmd}', data=json.dumps(a).encode(), headers={'X-WD': '1', 'Cookie': f'wd={T}', 'Content-Type': 'application/json'})
    j = json.loads(urllib.request.urlopen(r, timeout=60).read())
    if 'err' in j: raise Exception(j['err'])
    return j['ok']
U = 'me@gmail.com'
rpc('save_credentials', username=U, appPassword='x'); rpc('list_folders', username=U, refresh=True)
for f in ['INBOX', '[Gmail]/Sent Mail', '[Gmail]/All Mail']: rpc('sync_folder_now', username=U, folder=f)
def prefs(**k):
    rpc('save_preferences', preferences=dict(identity=U, page_size=25, **k))
from playwright.sync_api import sync_playwright
try:
    with sync_playwright() as pw:
        b = pw.chromium.launch()
        def page(theme='light'):
            prefs(theme=theme)
            pg = b.new_page(viewport={'width': 1280, 'height': 780}); pg.goto(f'http://127.0.0.1:{P}/?t={T}'); pg.wait_for_timeout(2500); return pg
        pg = page(); pg.screenshot(path=OUT + '/01-inbox.png')
        pg.locator('#mailRows > *').nth(1).click(); pg.wait_for_timeout(2000); pg.screenshot(path=OUT + '/02-message.png')
        pg.keyboard.press('Escape'); pg.click('#backThread') if pg.locator('#backThread').count() else None
        pg.wait_for_timeout(500); pg.click('#searchOpts'); pg.wait_for_timeout(600); pg.screenshot(path=OUT + '/03-advanced-search.png')
        pg.keyboard.press('Escape'); pg.click('#composeBtn'); pg.wait_for_timeout(1200); pg.screenshot(path=OUT + '/04-compose.png')
        pg.close()
        pg = page(); pg.click('#settingsBtn'); pg.wait_for_timeout(1200); pg.screenshot(path=OUT + '/05-settings.png'); pg.close()
        pg = page('dark'); pg.locator('#mailRows > *').nth(2).click(); pg.wait_for_timeout(1800); pg.screenshot(path=OUT + '/06-dark.png'); pg.close()
        b.close()
finally:
    app.kill(); imap.kill()
print(sorted(os.listdir(OUT)))
