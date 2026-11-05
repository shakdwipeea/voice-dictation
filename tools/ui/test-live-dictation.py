#!/usr/bin/env python3
"""Real dictations on the installed app: practice isolation, hold after a refused press, Quit's clipboard.

Usage: test-live-dictation.py [practice] [sleep] [hold] [quit]

Plays a phrase through the speakers and pastes only into its own disposable
window; it refuses to dictate if that window is not focused. Restarts and quits
Sunoto. The clipboard and config come back afterwards.
"""
import json, os, socket, subprocess, sys, tempfile, time
from pathlib import Path
WORK = Path(tempfile.mkdtemp(prefix="sunoto-live-"))
subprocess.run(["swiftc", "-O", str(Path(__file__).parent / "live-dictation-target.swift"), "-o", str(WORK / "target")], check=True)
SOCK = Path(subprocess.check_output(["getconf", "DARWIN_USER_TEMP_DIR"], text=True).strip()) / ("sunoto-" + os.environ["USER"] + "-daemon.sock")
CONFIG = Path.home() / "Library/Application Support/sunoto/config.json"
APP = Path.home() / "Applications/Sunoto.app"
PHRASE = "The next meeting is on Thursday at ten."
SENTINEL = "sunoto-clipboard-sentinel"

def send(line, reply=True):
    with socket.socket(socket.AF_UNIX) as c:
        c.settimeout(10); c.connect(str(SOCK)); c.sendall(line.encode() + b"\n")
        if not reply: return None
        data = b""
        while not data.endswith(b"\n"):
            b = c.recv(65536)
            if not b: break
            data += b
        return json.loads(data)
def status(): return send('{"type":"status"}')
def control(a): return send(json.dumps({"type":"user_control","action":a,"confirmed":True}))
def press(): send("press", reply=False)
def release(): send("release", reply=False)
def wait(pred, secs=15, what=""):
    end = time.monotonic() + secs; s = None
    while time.monotonic() < end:
        try:
            s = status()
            if pred(s): return s
        except OSError: pass
        time.sleep(.05)
    raise AssertionError(f"timed out: {what} {s}")
def clip(): return subprocess.check_output(["pbpaste"], text=True)
def setclip(t): subprocess.run(["pbcopy"], input=t, text=True, check=True)
def daemon_pids(): return subprocess.run(["pgrep","-f","Sunoto.app/Contents/MacOS/sunoto-daemon"],capture_output=True,text=True).stdout.split()
def stop_app():
    subprocess.run(["pkill","-f","Sunoto.app/Contents/MacOS/sunoto-daemon"])
    end = time.monotonic()+15
    while daemon_pids() and time.monotonic()<end: time.sleep(.1)
    assert not daemon_pids()
def start_app(): subprocess.run(["open",str(APP)],check=True)

class Target:
    def __init__(self):
        self.p = subprocess.Popen([str(WORK/"target")],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True,bufsize=1)
        assert self.p.stdout.readline().strip()=="ready"
    def ask(self,cmd,prefix):
        self.p.stdin.write(cmd+"\n"); self.p.stdin.flush()
        while True:
            l=self.p.stdout.readline()
            assert l, "target died"
            if l.startswith(prefix): return l.strip()[len(prefix):].strip()
    def ensure_focus(self):
        got=self.ask("focus","active")
        assert got=="true","disposable target is not focused; refusing to dictate"
    def still_focused(self): return self.ask("active","active")=="true"
    def text(self): return self.ask("text","text")
    def wait_change(self,secs):
        import select
        end=time.monotonic()+secs
        while time.monotonic()<end:
            r,_,_=select.select([self.p.stdout],[],[],.05)
            if r:
                l=self.p.stdout.readline()
                if l.startswith("changed"): return time.monotonic()
        return None
    def close(self):
        try: self.ask("quit","bye")
        except Exception: pass
        self.p.wait(timeout=5)

def speak(t):
    t.ensure_focus()
    wait(lambda s:s["session"]=="recording" and s["microphone"]=="capturing",10,"recording")
    subprocess.run(["say",PHRASE],check=True)
    time.sleep(.3)
    if not t.still_focused():
        control("interrupt"); raise AssertionError("target lost focus; session cancelled")

def practice_after_pause():
    t=Target(); out={}
    try:
        wait(lambda s:s["ready"]); setclip(SENTINEL); t.ensure_focus()
        before=status()["practice_results"]
        control("practice_start"); wait(lambda s:s["practice"],what="practice on")
        control("pause"); wait(lambda s:s["paused"],what="paused")
        control("resume"); wait(lambda s:not s["paused"],what="resumed"); time.sleep(.2)
        out["practice_after_resume"]=status()["practice"]
        press(); release(); speak(t); press(); release()
        changed=t.wait_change(8)
        s=wait(lambda s:s["session"]=="idle",20,"idle"); time.sleep(.5)
        out.update(pasted_into_target=bool(changed),target_text=t.text(),clipboard_is_sentinel=clip().strip()==SENTINEL,practice_results_added=s["practice_results"]-before)
    finally:
        control("practice_stop"); control("resume"); t.close()
    return out

def practice_after_sleep():
    t=Target(); out={}
    try:
        wait(lambda s:s["ready"]); setclip(SENTINEL); t.ensure_focus()
        before=status()["practice_results"]
        control("practice_start"); wait(lambda s:s["practice"],what="practice on")
        press(); release(); t.ensure_focus()
        wait(lambda s:s["session"]=="recording",10,"recording")
        control("interrupt"); s=wait(lambda s:s["session"]=="idle",10,"cancelled by sleep")
        out["mid_session_cancelled_mic"]=s["microphone"]; out["practice_after_sleep"]=s["practice"]
        press(); release(); speak(t); press(); release()
        changed=t.wait_change(8)
        s=wait(lambda s:s["session"]=="idle",20,"idle"); time.sleep(.5)
        out.update(pasted_into_target=bool(changed),target_text=t.text(),clipboard_is_sentinel=clip().strip()==SENTINEL,practice_results_added=s["practice_results"]-before)
    finally:
        control("practice_stop"); t.close()
    return out

def quit_right_after_paste():
    t=Target(); out={}
    try:
        wait(lambda s:s["ready"]); setclip(SENTINEL); t.ensure_focus()
        press(); release(); speak(t); press(); release()
        changed=t.wait_change(15)
        assert changed,"nothing was pasted"
        control("quit")
        out["quit_ms_after_paste"]=round((time.monotonic()-changed)*1000)
        end=time.monotonic()+20
        while daemon_pids() and time.monotonic()<end: time.sleep(.02)
        out["exit_s_after_quit"]=round(time.monotonic()-changed,2)
        out["daemon_gone"]=not daemon_pids()
        time.sleep(.5)
        out["clipboard"]=clip().strip()
        out["clipboard_restored"]=out["clipboard"]==SENTINEL
        out["target_text"]=t.text()
        left=subprocess.run("pgrep -fl 'sunoto|parakeet' | grep -v live.py || true",shell=True,capture_output=True,text=True).stdout.strip()
        out["processes_left"]=left
    finally:
        t.close()
    start_app(); wait(lambda s:s["ready"],90,"ready after relaunch")
    out["relaunched_ready"]=True
    return out

def hold_after_refused_press():
    backup=CONFIG.read_bytes(); out={}; t=None
    (WORK/"config.backup.json").write_bytes(backup)
    try:
        stop_app()
        cfg=json.loads(backup); cfg["interaction"]="hold"; CONFIG.write_text(json.dumps(cfg,indent=2))
        start_app()
        wait(lambda s:not s["ready"] and s["interaction"]=="hold",20,"warming in hold mode")
        press(); time.sleep(.1); release()
        s=status(); out["first_press_while_warming"]={"ready":s["ready"],"session":s["session"]}
        assert not s["ready"],"model warmed before the refused press"
        wait(lambda s:s["ready"],90,"ready")
        t=Target(); setclip(SENTINEL); t.ensure_focus()
        press()
        try:
            wait(lambda s:s["session"]=="recording",4,"hold recording"); out["next_press_records"]=True
        except AssertionError:
            out["next_press_records"]=False; release(); return out
        subprocess.run(["say",PHRASE],check=True); time.sleep(.3)
        if not t.still_focused(): control("interrupt"); raise AssertionError("target lost focus")
        release()
        out["pasted"]=bool(t.wait_change(15)); time.sleep(.6)
        out["target_text"]=t.text(); out["clipboard_restored"]=clip().strip()==SENTINEL
    finally:
        if t: t.close()
        stop_app(); CONFIG.write_bytes(backup); start_app()
        wait(lambda s:s["ready"],90,"ready after config restore")
    assert CONFIG.read_bytes()==backup
    out["config_restored"]=True
    return out

SCENARIOS={"practice":practice_after_pause,"sleep":practice_after_sleep,"quit":quit_right_after_paste,"hold":hold_after_refused_press}
for name in sys.argv[1:]:
    print(name,json.dumps(SCENARIOS[name](),indent=1),flush=True)
