"""Real endpoint picker, persisted fixed choice, and missing-device fallback notice."""
from ..app import App
from .. import win32 as w


def run(c):
    c.require_playback()
    app = App(c, input_kind="system")
    default_running = app.state()["input"]["running"]
    app.open_settings()
    app.click("Tab(Workspace)")
    app.click("InputPicker")
    app.click("InputChoice(1)")
    chosen = w.wait(lambda: app.state().get("input", {}).get("chosen")
                    if app.state().get("input", {}).get("chosen", {}).get("kind") == "system_endpoint" else None,
                    "first playback endpoint selected")
    state = w.wait(lambda: app.state() if app.state().get("input", {}).get("running") == chosen.get("name") else None,
                   "selected endpoint running")
    c.expect("picker_matches_trace", bool(chosen.get("device")) and state["input"]["running"] == chosen["name"]
             and state["input"]["device"] == chosen["device"], chosen=chosen, input=state["input"])
    app.quit()
    c.expect("selected_input_saved", app.read_local()["input"] == chosen,
             saved=app.read_local()["input"], chosen=chosen)
    restarted = App(c, "restart", config=app.config, input_kind="system")
    state = restarted.state()
    c.expect("restart_keeps_choice", state["input"]["chosen"] == chosen and state["input"]["running"] == chosen["name"], input=state["input"])
    restarted.quit()
    local = restarted.read_local()
    missing = {"kind": "system_endpoint", "device": "wasapi:{0.0.0.00000000}.{00000000-0000-0000-0000-000000000000}",
               "name": "Native smoke missing endpoint"}
    local["input"] = missing
    restarted.write_local(local)
    fallback = App(c, "missing", config=app.config, input_kind="system")
    state = w.wait(lambda: fallback.state() if fallback.state().get("notices") else None,
                   "unavailable input notice")
    c.expect("missing_falls_back_and_notifies", state["input"]["chosen"] == missing
             and state["input"]["running"] == default_running
             and state["source"] == "Running", input=state["input"], notices=state["notices"],
             source=state["source"], default_running=default_running)
    fallback.quit()
    c.expect("missing_choice_preserved", fallback.read_local()["input"] == missing,
             saved=fallback.read_local()["input"])
