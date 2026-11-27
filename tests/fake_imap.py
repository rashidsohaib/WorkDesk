#!/usr/bin/env python3
"""Tiny fake Gmail-like IMAP server (plaintext) for WorkDesk backend tests. Logs every command to $LOG."""
import os, re, socket, sys, threading, time
from email.message import EmailMessage
from email.utils import format_datetime
import datetime

PORT = int(sys.argv[1]); LOG = open(sys.argv[2], 'a', buffering=1)
FOLDERS = [('INBOX', ''), ('[Gmail]/Sent Mail', '\\Sent'), ('[Gmail]/All Mail', '\\All'), ('[Gmail]/Drafts', '\\Drafts'), ('[Gmail]/Trash', '\\Trash')]
lock = threading.Lock()
MSGS = {}      # gm_msgid -> dict(raw, thrid, date)
STORE = {f: {} for f, _ in FOLDERS}   # folder -> uid -> dict(msgid, flags)
NEXT = {f: 1 for f, _ in FOLDERS}
VALIDITY = 7

NAMES = ['Aisha Rahman', 'Daniel Lim', 'Priya Nair', 'Marcus Tan', 'Sofia Alvarez', 'Kenji Watanabe', 'Hannah Wong', 'Omar Haddad', 'Grace Lee', 'Ivan Petrov', 'Nur Aina', 'Ben Carter', 'Mei Ling', 'Luca Rossi']
SUBJ = ['Q4 budget review', 'Invoice 2026-1187 attached', 'Lunch on Thursday?', 'Project Falcon: status update', 'Re: Onboarding checklist', 'Meeting notes - 12 Nov', 'Contract renewal draft', 'Quarterly newsletter', 'Re: Server maintenance window', 'Photos from the offsite', 'Travel itinerary - Singapore', 'Re: Support ticket #4821', 'Welcome to the team!', 'Weekly report', ]
def mk(i, subject, inline=False):
    DEMO = bool(os.environ.get('WD_DEMO'))
    subject = SUBJ[(i - 1) % len(SUBJ)] if DEMO and subject.startswith('Subject') else subject
    m = EmailMessage(); m['From'] = (f'{NAMES[(i - 1) % len(NAMES)]} <s{i}@example.com>' if DEMO else f'Sender {i} <s{i}@example.com>'); m['To'] = 'me@gmail.com'
    m['Subject'] = subject; m['Message-ID'] = f'<m{i}@example.com>'
    m['Date'] = format_datetime(datetime.datetime(2026, 11, 1, 10, 0, 0, tzinfo=datetime.timezone.utc) + datetime.timedelta(minutes=i))
    m.set_content(f'plain body {i}' if not DEMO else f'Hi Rashid,\n\nThanks for your message about this. Please find the details below and let me know if anything is unclear.\n\nBest regards')
    body_html = f'<p>html body {i}</p>' if not DEMO else f'<p>Hi Rashid,</p><p>Thanks for your message about this. Please find the details below and let me know if anything is unclear.</p><ul><li>Item one is confirmed</li><li>Item two is pending approval</li></ul><p>Best regards,<br><b>{NAMES[(i - 1) % len(NAMES)]}</b></p>'
    m.add_alternative(body_html + ('<img src="cid:ii_abc">' if inline else ''), subtype='html')
    if inline:
        m.get_payload()[1].add_related(b'\x89PNG\r\n\x1a\n' + b'0' * 300, 'image', 'png', cid='<ii_abc>', disposition='attachment', filename='image.png')
    return m.as_bytes()

def add(folder, msgid, flags=()):
    uid = NEXT[folder]; NEXT[folder] += 1
    STORE[folder][uid] = {'msgid': msgid, 'flags': set(flags)}
    return uid

def seed():
    for i in range(1, 13):
        msgid = 1000 + i
        MSGS[msgid] = {'raw': mk(i, f'Subject {i}', inline=(i == 12)), 'thrid': 500 + i, 'ts': 1793500000 + i * 60}
        add('[Gmail]/All Mail', msgid, ['\\Seen'] if i % 3 == 0 else [])
        if i <= 6: add('INBOX', msgid, ['\\Seen'] if i % 3 == 0 else [])
        if i > 9: add('[Gmail]/Sent Mail', msgid, ['\\Seen'])
seed()
for i in (13, 14):
    MSGS[1000 + i] = {'raw': mk(i, f'Subject {i}'), 'thrid': 500 + i, 'ts': 1793500000 + i * 60}
    add('[Gmail]/Trash', 1000 + i)
seed_done = True

def fmt_flags(fl): return '(' + ' '.join(sorted(fl)) + ')'
def parse_set(s, maxuid):
    out = []
    for part in s.split(','):
        if ':' in part:
            a, b = part.split(':'); a = int(a); b = maxuid if b == '*' else int(b)
            out += list(range(min(a, b), max(a, b) + 1))
        else: out.append(int(part))
    return out

def envelope(raw, msg):
    import email
    em = email.message_from_bytes(raw)
    name, addr = email.utils.parseaddr(em['From']); user, host = addr.split('@')
    subj = em['Subject'].replace('"', '')
    return f'("{em["Date"]}" "{subj}" (("{name}" NIL "{user}" "{host}")) (("{name}" NIL "{user}" "{host}")) (("{name}" NIL "{user}" "{host}")) ((NIL NIL "me" "gmail.com")) NIL NIL NIL "{em["Message-ID"]}")'

def serve(conn):
    f = conn.makefile('rwb', buffering=0)
    def w(b):
        if isinstance(b, str): b = b.encode()
        f.write(b)
    w('* OK IMAP4rev1 fake ready\r\n')
    sel = None
    while True:
        line = f.readline()
        if not line: break
        line = line.decode('utf8', 'replace').rstrip('\r\n')
        if not line: continue
        tag, _, rest = line.partition(' ')
        cmd = rest.split(' ', 1)[0].upper(); args = rest[len(cmd):].strip()
        LOG.write(f'{time.time():.3f} [{threading.get_ident()%1000}] {sel} :: {rest[:150]}\n')
        with lock:
            if cmd == 'CAPABILITY': w('* CAPABILITY IMAP4rev1 X-GM-EXT-1 UIDPLUS MOVE\r\n'); w(f'{tag} OK done\r\n')
            elif cmd == 'LOGIN': w(f'{tag} OK logged in\r\n')
            elif cmd == 'NOOP': w(f'{tag} OK noop\r\n')
            elif cmd == 'LOGOUT': w('* BYE bye\r\n'); w(f'{tag} OK bye\r\n'); break
            elif cmd == 'LIST':
                for n, a in FOLDERS: w(f'* LIST (\\HasNoChildren {a}) "/" "{n}"\r\n')
                w(f'{tag} OK done\r\n')
            elif cmd in ('SELECT', 'EXAMINE'):
                sel = args.strip('"'); st = STORE[sel]
                w(f'* {len(st)} EXISTS\r\n* 0 RECENT\r\n* OK [UIDVALIDITY {VALIDITY}] ok\r\n* OK [UIDNEXT {NEXT[sel]}] ok\r\n* FLAGS (\\Seen \\Flagged \\Draft \\Deleted \\Answered)\r\n{tag} OK [READ-WRITE] done\r\n')
            elif cmd == 'UID':
                sub, _, a2 = args.partition(' '); sub = sub.upper(); st = STORE[sel]; uids_sorted = sorted(st)
                if sub == 'SEARCH':
                    if 'X-GM-THRID' in a2.upper():
                        t = int(a2.split()[-1]); res = [u for u in uids_sorted if MSGS[st[u]['msgid']]['thrid'] == t]
                    else: res = uids_sorted
                    w('* SEARCH ' + ' '.join(map(str, res)) + '\r\n' if res else '* SEARCH\r\n'); w(f'{tag} OK done\r\n')
                elif sub == 'FETCH':
                    setspec, _, items = a2.partition(' ')
                    for u in parse_set(setspec, max(uids_sorted) if uids_sorted else 0):
                        if u not in st: continue
                        e = st[u]; m = MSGS[e['msgid']]; seq = uids_sorted.index(u) + 1
                        parts = [f'UID {u}']
                        if 'FLAGS' in items: parts.append('FLAGS ' + fmt_flags(e['flags']))
                        if 'RFC822.SIZE' in items: parts.append(f'RFC822.SIZE {len(m["raw"])}')
                        if 'INTERNALDATE' in items:
                            d = datetime.datetime.fromtimestamp(m['ts'], datetime.timezone.utc).strftime('%d-%b-%Y %H:%M:%S +0000'); parts.append(f'INTERNALDATE "{d}"')
                        if 'ENVELOPE' in items: parts.append('ENVELOPE ' + envelope(m['raw'], m))
                        if 'BODYSTRUCTURE' in items: parts.append('BODYSTRUCTURE ("TEXT" "PLAIN" ("CHARSET" "utf-8") NIL NIL "7BIT" 10 1)')
                        if 'X-GM-THRID' in items: parts.append(f'X-GM-THRID {m["thrid"]}')
                        if 'X-GM-MSGID' in items: parts.append(f'X-GM-MSGID {e["msgid"]}')
                        if 'BODY.PEEK[]' in items or 'BODY[]' in items:
                            w(f'* {seq} FETCH (' + ' '.join(parts) + f' BODY[] {{{len(m["raw"])}}}\r\n'); w(m['raw']); w(')\r\n')
                        else: w(f'* {seq} FETCH (' + ' '.join(parts) + ')\r\n')
                    w(f'{tag} OK done\r\n')
                elif sub == 'STORE':
                    setspec, _, rem = a2.partition(' '); op, _, fl = rem.partition(' ')
                    flags = set(re.findall(r'\\\w+', fl))
                    for u in parse_set(setspec, max(uids_sorted)):
                        if u in st:
                            if op.startswith('+'): st[u]['flags'] |= flags
                            elif op.startswith('-'): st[u]['flags'] -= flags
                            else: st[u]['flags'] = flags
                    w(f'{tag} OK done\r\n')
                elif sub == 'COPY':
                    setspec, _, dest = a2.partition(' '); dest = dest.strip('"'); newu = []
                    for u in parse_set(setspec, max(uids_sorted)):
                        if u in st: newu.append(add(dest, st[u]['msgid'], st[u]['flags']))
                    w(f'{tag} OK [COPYUID {VALIDITY} {setspec} {",".join(map(str,newu))}] done\r\n')
                elif sub == 'EXPUNGE':
                    for u in parse_set(a2, max(uids_sorted)):
                        if u in st and '\\Deleted' in st[u]['flags']: del st[u]
                    w(f'{tag} OK done\r\n')
                elif sub in ('MOVE',):
                    setspec, _, dest = a2.partition(' '); dest = dest.strip('"')
                    for u in parse_set(setspec, max(uids_sorted)):
                        if u in st: add(dest, st[u]['msgid'], st[u]['flags']); del st[u]
                    w(f'{tag} OK done\r\n')
                else: w(f'{tag} BAD unsupported uid {sub}\r\n')
            elif cmd == 'APPEND':
                m = re.match(r'"?([^"]+?)"?\s*(\(.*?\))?\s*(".*?")?\s*\{(\d+)\+?\}', args)
                folder, fl, n = m.group(1), m.group(2) or '', int(m.group(4))
                w('+ go ahead\r\n'); data = b''
                while len(data) < n: data += f.read(n - len(data))
                f.readline()
                msgid = 9000 + len(MSGS); MSGS[msgid] = {'raw': data, 'thrid': msgid, 'ts': int(time.time())}
                u = add(folder, msgid, set(re.findall(r'\\\w+', fl)))
                w(f'{tag} OK [APPENDUID {VALIDITY} {u}] done\r\n')
            elif cmd == 'EXPUNGE':
                st = STORE[sel]
                for u in [u for u, e in st.items() if '\\Deleted' in e['flags']]: del st[u]
                w(f'{tag} OK done\r\n')
            else: w(f'{tag} BAD unknown {cmd}\r\n')
    conn.close()

s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind(('127.0.0.1', PORT)); s.listen(50)
while True:
    c, _ = s.accept(); threading.Thread(target=serve, args=(c,), daemon=True).start()
