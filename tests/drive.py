import json, os, subprocess, sys, time, urllib.request, signal, re, glob
BIN = sys.argv[1]; HOME = '/tmp/wdtest'; PORT_IMAP = 14143
subprocess.run(['rm', '-rf', HOME]); os.makedirs(HOME + '/data'); os.makedirs(HOME + '/cfg')
LOG = HOME + '/imap.log'
imap = subprocess.Popen([sys.executable, '/home/claude/wd/tests/fake_imap.py', str(PORT_IMAP), LOG])
time.sleep(0.8)
env = dict(os.environ, HOME=HOME, XDG_DATA_HOME=HOME + '/data', XDG_CONFIG_HOME=HOME + '/cfg', WORKDESK_SHELL='electron', MAILDESK_IMAP_ADDR=f'127.0.0.1:{PORT_IMAP}')
app = subprocess.Popen([BIN], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=open(HOME + '/app.err', 'w'))
ready = json.loads(app.stdout.readline().decode().split(' ', 1)[1]); P, T = ready['port'], ready['token']
def rpc(cmd, **a):
    r = urllib.request.Request(f'http://127.0.0.1:{P}/rpc/{cmd}', data=json.dumps(a).encode(), headers={'X-WD': '1', 'Cookie': f'wd={T}', 'Content-Type': 'application/json'})
    j = json.loads(urllib.request.urlopen(r, timeout=90).read())
    if 'err' in j: raise Exception(f'{cmd}: {j["err"]}')
    return j['ok']
U = 'me@gmail.com'; ok = True
def check(name, cond, extra=''):
    global ok; ok &= bool(cond); print(('PASS ' if cond else 'FAIL ') + name, extra)
def log_lines(): return open(LOG).read().splitlines()
try:
    rpc('save_credentials', username=U, appPassword='abcd efgh ijkl mnop')
    check('vault files written', os.path.exists(HOME + '/cfg/com.workdesk.app/vault.json') and os.path.exists(HOME + '/cfg/com.workdesk.app/vault.key'))
    fl = rpc('list_folders', username=U, refresh=True); names = [f['name'] for f in fl['folders']]; check('list_folders', 'INBOX' in names and '[Gmail]/All Mail' in names, names)
    for f in ['INBOX', '[Gmail]/Sent Mail', '[Gmail]/All Mail', '[Gmail]/Drafts']: rpc('sync_folder_now', username=U, folder=f)
    pg = rpc('list_page', username=U, folder='INBOX', grouped=False, sort='date', desc=True, page=0, pageSize=50); check('inbox headers cached', pg['total'] == 6, pg['total'])
    n0 = len(log_lines()); rpc('warm_inbox', username=U); time.sleep(4); new = log_lines()[n0:]
    body = [l for l in new if 'BODY.PEEK[]' in l]; uids = set()
    for l in body:
        m = re.search(r'UID FETCH (\S+)', l); uids.add((l.split('::')[0].split()[0], m.group(1)))
    total_fetched = 0
    for l in body:
        spec = re.search(r'UID FETCH (\S+)', l).group(1)
        total_fetched += len([x for p in spec.split(',') for x in (range(int(p.split(':')[0]), int(p.split(':')[1]) + 1) if ':' in p else [1])])
    check('warm downloads each distinct message once (12 msgs, shared across labels)', total_fetched == 12, f'fetched={total_fetched}')
    n1 = len(log_lines()); t = time.time(); d = rpc('get_message', username=U, folder='INBOX', uid=1); dt = time.time() - t
    time.sleep(1); after = log_lines()[n1:]
    check('open warmed message: instant, no body download', dt < 0.2 and not any('BODY.PEEK[]' in l for l in after), f'{dt*1000:.0f}ms')
    check('decoded content', 'html body 1' in d['html'] and d['subject'] == 'Subject 1')
    check('Seen stored on server in background', any('STORE' in l and 'Seen' in l for l in after))
    allm0 = rpc('list_page', username=U, folder='[Gmail]/All Mail', grouped=False, sort='date', desc=True, page=0, pageSize=50)['items']
    a3 = [i for i in allm0 if i['subject'] == 'Subject 3'][0]
    n2 = len(log_lines()); t = time.time(); d3 = rpc('get_message', username=U, folder='[Gmail]/All Mail', uid=a3['uid']); dt3 = time.time() - t
    check('thread-view copy (All Mail uid) of an Inbox-warmed message opens from memory', dt3 < 0.2 and 'html body 3' in d3['html'] and not any('BODY.PEEK[]' in l for l in log_lines()[n2:]), f'{dt3*1000:.0f}ms')
    rpc('sync_folder_now', username=U, folder='[Gmail]/Trash')
    tr = rpc('list_page', username=U, folder='[Gmail]/Trash', grouped=False, sort='date', desc=True, page=0, pageSize=50)['items']
    logins0 = sum(1 for l in log_lines() if ':: LOGIN' in l)
    for it in tr: rpc('get_message', username=U, folder='[Gmail]/Trash', uid=it['uid'], markSeen=False)
    logins1 = sum(1 for l in log_lines() if ':: LOGIN' in l)
    check('uncached messages reuse a ready connection (no new login per message)', logins1 - logins0 <= 1, f'logins {logins0}->{logins1} for {len(tr)} messages')
    sent = rpc('list_page', username=U, folder='[Gmail]/Sent Mail', grouped=False, sort='date', desc=True, page=0, pageSize=50)['items']
    top = [i for i in sent if i['subject'] == 'Subject 12'][0]
    d2 = rpc('get_message', username=U, folder='[Gmail]/Sent Mail', uid=top['uid'], markSeen=False)
    a = d2['attachments']; check('Gmail-web style inline image flagged inline with data', len(a) == 1 and a[0]['inline'] and a[0]['contentId'] == 'ii_abc' and a[0]['dataBase64'], a and (a[0]['inline'], a[0]['contentId']))
    allm = rpc('list_page', username=U, folder='[Gmail]/All Mail', grouped=False, sort='date', desc=True, page=0, pageSize=50)['items']
    s7 = [i for i in allm if i['subject'] == 'Subject 7'][0]
    rpc('add_to_inbox', username=U, refs=[{'folder': '[Gmail]/All Mail', 'uid': s7['uid']}])
    inbox_after = rpc('list_page', username=U, folder='INBOX', grouped=False, sort='date', desc=True, page=0, pageSize=50)
    check('Move to Inbox adds label, keeps original', inbox_after['total'] == 7 and any(i['subject'] == 'Subject 7' for i in inbox_after['items']))
    check('no expunge/delete issued by add_to_inbox', not any('EXPUNGE' in l or 'Deleted' in l for l in log_lines()[-12:]))
    tm = rpc('thread_messages', username=U, threadId=str(503), folder='INBOX', uid=3, refresh=False); check('thread_messages (cache only)', len(tm) >= 1)
    tm2 = rpc('thread_messages', username=U, threadId=str(503), folder='INBOX', uid=3, refresh=True); check('thread_messages (refresh)', len(tm2) >= 1)
    req = dict(username=U, fromEmail=None, fromName='Me', to='x@example.com', subject='Draft test', text='hi', html='<p>hi</p>')
    s1 = rpc('save_draft', req=req, replaceUid=None, replaceValidity=None); check('save_draft returns uid/validity', s1['uid'] and s1['validity'] == 7, s1)
    v = rpc('folder_validity', username=U, folder='[Gmail]/Drafts'); check('folder_validity', v == 7, v)
    s2 = rpc('save_draft', req=req, replaceUid=s1['uid'], replaceValidity=s1['validity'])
    rpc('discard_draft', username=U, folder='[Gmail]/Drafts', uid=s2['uid'], validity=s2['validity'])
    dr = rpc('sync_folder_now', username=U, folder='[Gmail]/Drafts'); pgd = rpc('list_page', username=U, folder='[Gmail]/Drafts', grouped=False, sort='date', desc=True, page=0, pageSize=50)
    check('draft discarded (replace + discard leave none)', pgd['total'] == 0, pgd['total'])
    # scheduler: SMTP is unreachable here, so this checks launch, claim, failure bookkeeping and surviving the app
    req2 = dict(username=U, fromEmail=None, fromName='Me', to='x@example.com', subject='Later', text='hi', html=None)
    sid = rpc('schedule_send', req=req2, sendAt=int(time.time() * 1000) + 1500)
    time.sleep(1); pidf = HOME + '/data/com.workdesk.app/sender.pid'
    check('detached sender started', os.path.exists(pidf), open(pidf).read() if os.path.exists(pidf) else '')
    spid = int(open(pidf).read()); cmd = open(f'/proc/{spid}/cmdline').read().replace('\0', ' ')
    check('sender runs from private copy', 'workdesk-sender' in cmd and '--send-scheduled' in cmd, cmd)
    time.sleep(12)
    ls = rpc('list_scheduled'); check('failed send is kept with attempts/error', ls and ls[0]['attempts'] >= 1 and ls[0]['error'], ls)
    app.stdin.close(); app.wait(timeout=10); time.sleep(1)
    check('sender survives WorkDesk exit', os.path.exists(f'/proc/{spid}'))
finally:
    for f in glob.glob(HOME + '/data/com.workdesk.app/sender.pid'):
        try: os.kill(int(open(f).read()), signal.SIGTERM)
        except Exception: pass
    app.kill(); imap.kill()
print('ALL OK' if ok else 'SOME FAILED'); print(open(HOME + '/app.err').read()[-800:])
