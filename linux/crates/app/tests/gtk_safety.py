#!/usr/bin/env python3
import ctypes
import json
import os
import shutil
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import pyatspi

APP_NAME = "TablePro"
CONNECTION_NAME = "Safety SQLite"
WAIT_SECONDS = 15
POLL_SECONDS = 0.05
SETTLE_SECONDS = 3.0


def descendants(node):
    yield node
    for child in node:
        yield from descendants(child)


def node_name(node):
    try:
        return node.name or ""
    except Exception:
        return ""


def node_role(node):
    try:
        return node.getRole()
    except Exception:
        return pyatspi.ROLE_INVALID


def application_node():
    desktop = pyatspi.Registry.getDesktop(0)
    for application in desktop:
        names = {node_name(application)}
        names.update(node_name(node) for node in descendants(application))
        if APP_NAME in names or any(CONNECTION_NAME in name for name in names):
            return application
    return None


def find_node(name=None, role=None):
    application = application_node()
    if application is None:
        return None
    for node in descendants(application):
        if name is not None and node_name(node) != name:
            continue
        if role is not None and node_role(node) != role:
            continue
        return node
    return None


def accessible_snapshot():
    application = application_node()
    if application is None:
        return "application unavailable"
    entries = []
    for node in descendants(application):
        name = node_name(node)
        if not name:
            continue
        try:
            role_name = node.getRoleName()
        except Exception:
            role_name = "unknown"
        try:
            extents = node.queryComponent().getExtents(pyatspi.DESKTOP_COORDS)
            bounds = f" [{extents.x},{extents.y} {extents.width}x{extents.height}]"
        except Exception:
            bounds = ""
        try:
            action = node.queryAction()
            actions = f" actions={[action.getName(index) for index in range(action.nActions)]}"
        except Exception:
            actions = ""
        entries.append(f"{role_name}: {name}{bounds}{actions}")
    return "\n".join(entries)


def wait_for_node(name=None, role=None, present=True, timeout=WAIT_SECONDS):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        node = find_node(name=name, role=role)
        if (node is not None) == present:
            return node
        time.sleep(POLL_SECONDS)
    expectation = "appear" if present else "disappear"
    raise AssertionError(f"expected accessible node {name!r} to {expectation}:\n{accessible_snapshot()}")


def invoke(node):
    candidates = []
    candidate = node
    for _ in range(3):
        if candidate is None:
            break
        candidates.append(candidate)
        try:
            candidate = candidate.parent
        except Exception:
            break
    for candidate in candidates:
        try:
            action = candidate.queryAction()
            for index in range(action.nActions):
                action_name = action.getName(index).lower()
                if action_name in {"click", "activate", "press", "toggle"} and action.doAction(index):
                    return
        except Exception:
            continue
    for candidate in candidates:
        if not node_is_focusable_button(candidate):
            continue
        try:
            if not candidate.queryComponent().grabFocus():
                continue
        except Exception:
            continue
        if not wait_for_focus(candidate):
            continue
        pyatspi.Registry.generateKeyboardEvent(0, "Return", pyatspi.KEY_SYM)
        return
    raise AssertionError(f"no invokable action for {node_name(node)!r}")


def node_is_focusable_button(node):
    if node_role(node) != pyatspi.ROLE_PUSH_BUTTON:
        return False
    try:
        return node.getState().contains(pyatspi.STATE_FOCUSABLE)
    except Exception:
        return False


def node_has_focus(node):
    try:
        return node.getState().contains(pyatspi.STATE_FOCUSED)
    except Exception:
        return False


def wait_for_focus(node, timeout=2.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if node_has_focus(node):
            return True
        time.sleep(POLL_SECONDS)
    return False


def press_x11_key(name, modifiers=()):
    class XWindowAttributes(ctypes.Structure):
        _fields_ = [
            ("x", ctypes.c_int),
            ("y", ctypes.c_int),
            ("width", ctypes.c_int),
            ("height", ctypes.c_int),
            ("border_width", ctypes.c_int),
            ("depth", ctypes.c_int),
            ("visual", ctypes.c_void_p),
            ("root", ctypes.c_ulong),
            ("window_class", ctypes.c_int),
            ("bit_gravity", ctypes.c_int),
            ("win_gravity", ctypes.c_int),
            ("backing_store", ctypes.c_int),
            ("backing_planes", ctypes.c_ulong),
            ("backing_pixel", ctypes.c_ulong),
            ("save_under", ctypes.c_int),
            ("colormap", ctypes.c_ulong),
            ("map_installed", ctypes.c_int),
            ("map_state", ctypes.c_int),
            ("all_event_masks", ctypes.c_long),
            ("your_event_mask", ctypes.c_long),
            ("do_not_propagate_mask", ctypes.c_long),
            ("override_redirect", ctypes.c_int),
            ("screen", ctypes.c_void_p),
        ]

    x11 = ctypes.CDLL("libX11.so.6")
    xtst = ctypes.CDLL("libXtst.so.6")
    x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
    x11.XOpenDisplay.restype = ctypes.c_void_p
    x11.XStringToKeysym.argtypes = [ctypes.c_char_p]
    x11.XStringToKeysym.restype = ctypes.c_ulong
    x11.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
    x11.XKeysymToKeycode.restype = ctypes.c_uint
    x11.XDefaultRootWindow.argtypes = [ctypes.c_void_p]
    x11.XDefaultRootWindow.restype = ctypes.c_ulong
    x11.XQueryTree.argtypes = [
        ctypes.c_void_p,
        ctypes.c_ulong,
        ctypes.POINTER(ctypes.c_ulong),
        ctypes.POINTER(ctypes.c_ulong),
        ctypes.POINTER(ctypes.POINTER(ctypes.c_ulong)),
        ctypes.POINTER(ctypes.c_uint),
    ]
    x11.XGetWindowAttributes.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.POINTER(XWindowAttributes)]
    x11.XSetInputFocus.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_ulong]
    x11.XFree.argtypes = [ctypes.c_void_p]
    x11.XFlush.argtypes = [ctypes.c_void_p]
    x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
    xtst.XTestFakeKeyEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
    display = x11.XOpenDisplay(None)
    if not display:
        raise AssertionError("X11 display is unavailable")
    try:
        keysym = x11.XStringToKeysym(name.encode("ascii"))
        keycode = x11.XKeysymToKeycode(display, keysym)
        if keycode == 0:
            raise AssertionError(f"X11 key is unavailable: {name}")
        root = x11.XDefaultRootWindow(display)
        returned_root = ctypes.c_ulong()
        returned_parent = ctypes.c_ulong()
        children = ctypes.POINTER(ctypes.c_ulong)()
        child_count = ctypes.c_uint()
        status = x11.XQueryTree(
            display,
            root,
            ctypes.byref(returned_root),
            ctypes.byref(returned_parent),
            ctypes.byref(children),
            ctypes.byref(child_count),
        )
        if status == 0 or child_count.value == 0:
            raise AssertionError("X11 application window is unavailable")
        try:
            focus_window = None
            for index in range(child_count.value - 1, -1, -1):
                attributes = XWindowAttributes()
                if x11.XGetWindowAttributes(display, children[index], ctypes.byref(attributes)) and attributes.map_state == 2:
                    focus_window = children[index]
                    break
            if focus_window is None:
                raise AssertionError("viewable X11 application window is unavailable")
            x11.XSetInputFocus(display, focus_window, 2, 0)
        finally:
            x11.XFree(children)
        modifier_codes = []
        for modifier in modifiers:
            modifier_keysym = x11.XStringToKeysym(modifier.encode("ascii"))
            modifier_code = x11.XKeysymToKeycode(display, modifier_keysym)
            if modifier_code == 0:
                raise AssertionError(f"X11 modifier is unavailable: {modifier}")
            modifier_codes.append(modifier_code)
        for modifier_code in modifier_codes:
            xtst.XTestFakeKeyEvent(display, modifier_code, 1, 0)
        xtst.XTestFakeKeyEvent(display, keycode, 1, 0)
        xtst.XTestFakeKeyEvent(display, keycode, 0, 0)
        for modifier_code in reversed(modifier_codes):
            xtst.XTestFakeKeyEvent(display, modifier_code, 0, 0)
        x11.XFlush(display)
    finally:
        x11.XCloseDisplay(display)


def set_editor_text(sql):
    application = application_node()
    if application is None:
        raise AssertionError("application is not available")
    candidates = []
    for node in descendants(application):
        if node_role(node) != pyatspi.ROLE_TEXT:
            continue
        try:
            editable = node.queryEditableText()
            candidates.append((node, editable))
        except Exception:
            continue
    if not candidates:
        raise AssertionError("SQL editor text control was not found")
    node, editable = max(candidates, key=lambda candidate: candidate[0].queryText().characterCount)
    editable.setTextContents(sql)
    return node


def database_row_count(path):
    with sqlite3.connect(path) as connection:
        return connection.execute("SELECT COUNT(*) FROM safety_items").fetchone()[0]


def wait_for_database_count(path, expected, timeout=WAIT_SECONDS):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if database_row_count(path) == expected:
            return
        time.sleep(POLL_SECONDS)
    actual = database_row_count(path)
    raise AssertionError(f"expected {expected} database rows, found {actual}")


def assert_database_count_stable(path, expected, seconds=SETTLE_SECONDS):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        actual = database_row_count(path)
        if actual != expected:
            raise AssertionError(f"expected {expected} database rows for {seconds}s, found {actual}")
        time.sleep(POLL_SECONDS)


def set_text_by_name(name, text, timeout=WAIT_SECONDS):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        application = application_node()
        if application is not None:
            for node in descendants(application):
                if node_name(node) != name or node_role(node) != pyatspi.ROLE_TEXT:
                    continue
                try:
                    editable = node.queryEditableText()
                except Exception:
                    continue
                editable.setTextContents(text)
                return
        time.sleep(POLL_SECONDS)
    raise AssertionError(f"no editable text control named {name!r}:\n{accessible_snapshot()}")


def database_ids(path):
    with sqlite3.connect(path) as connection:
        return [row[0] for row in connection.execute("SELECT id FROM safety_items ORDER BY id")]


def write_fixture(base, audit_available=True, environment="prod"):
    home = base / "home"
    config = base / "config"
    data = base / "data"
    cache = base / "cache"
    state = base / "state"
    runtime = base / "runtime"
    for directory in [home, config, cache, state, runtime]:
        directory.mkdir(parents=True)
    runtime.chmod(0o700)
    if audit_available:
        data.mkdir(parents=True)
    else:
        data.write_text("audit unavailable", encoding="utf-8")

    database = base / "safety.sqlite"
    with sqlite3.connect(database) as connection:
        connection.execute("CREATE TABLE safety_items (id INTEGER PRIMARY KEY)")

    tablepro_config = config / "tablepro"
    tablepro_config.mkdir(parents=True)
    connections = {
        "version": 1,
        "connections": [
            {
                "id": "d4f5e246-c40a-4a30-90f2-39e93e94d920",
                "name": CONNECTION_NAME,
                "driver_id": "sqlite",
                "host": "",
                "port": 0,
                "database": str(database),
                "username": "",
                "use_tls": False,
                "tls_mode": "disabled",
                "read_only": False,
                "auth_mode": "password",
                "environment": environment,
            }
        ],
    }
    (tablepro_config / "connections.json").write_text(json.dumps(connections), encoding="utf-8")

    environment = os.environ.copy()
    environment.update(
        {
            "HOME": str(home),
            "XDG_CONFIG_HOME": str(config),
            "XDG_DATA_HOME": str(data),
            "XDG_CACHE_HOME": str(cache),
            "XDG_STATE_HOME": str(state),
            "XDG_RUNTIME_DIR": str(runtime),
            "RUST_LOG": "tablepro_app=debug",
        }
    )
    return database, environment


def start_application(binary, environment):
    process = subprocess.Popen(
        [str(binary)],
        env=environment,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        wait_for_node(name=CONNECTION_NAME)
        return process
    except Exception:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
        stderr = process.stderr.read() if process.stderr is not None else ""
        raise AssertionError(f"application did not expose the welcome screen: {stderr}")


def stop_application(process):
    if process.poll() is None:
        process.send_signal(signal.SIGTERM)
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
    stderr = process.stderr.read() if process.stderr is not None else ""
    if process.returncode not in {0, -signal.SIGTERM}:
        raise AssertionError(f"application exited with {process.returncode}: {stderr}")
    return stderr


def open_editor():
    invoke(wait_for_node(name="Open connection", role=pyatspi.ROLE_PUSH_BUTTON))
    invoke(wait_for_node(name="Open SQL editor"))
    wait_for_node(name="Run", role=pyatspi.ROLE_PUSH_BUTTON)


def run_sql(sql):
    set_editor_text(sql)
    invoke(wait_for_node(name="Run", role=pyatspi.ROLE_PUSH_BUTTON))


def run_scenario(binary, scenario):
    base = Path(tempfile.mkdtemp(prefix=f"tablepro-{scenario.__name__}-"))
    process = None
    failure = None
    stderr = ""
    try:
        database, environment = write_fixture(
            base,
            audit_available=getattr(scenario, "audit_available", True),
            environment=getattr(scenario, "environment", "prod"),
        )
        process = start_application(binary, environment)
        open_editor()
        scenario(database, base)
    except Exception as error:
        failure = error
    finally:
        if process is not None:
            stderr = stop_application(process)
        shutil.rmtree(base, ignore_errors=True)
    if failure is not None:
        raise AssertionError(f"{failure}\napplication stderr:\n{stderr}") from failure


def dismissed_approval_denies(database, _base):
    run_sql("INSERT INTO safety_items(id) VALUES (1)")
    wait_for_node(name="Approve once", role=pyatspi.ROLE_PUSH_BUTTON)
    time.sleep(SETTLE_SECONDS / 3)
    if find_node(name="Approve once", role=pyatspi.ROLE_PUSH_BUTTON) is None:
        raise AssertionError("the approval dialog closed before the harness dismissed it")
    press_x11_key("Escape")
    wait_for_node(name="Approve once", present=False)
    assert_database_count_stable(database, 0)


def approve_once_prompts_again(database, _base):
    run_sql("INSERT INTO safety_items(id) VALUES (1)")
    invoke(wait_for_node(name="Approve once", role=pyatspi.ROLE_PUSH_BUTTON))
    wait_for_node(name="Approve once", present=False)
    wait_for_database_count(database, 1)

    run_sql("INSERT INTO safety_items(id) VALUES (2)")
    wait_for_node(name="Approve once", role=pyatspi.ROLE_PUSH_BUTTON)
    invoke(wait_for_node(name="Deny", role=pyatspi.ROLE_PUSH_BUTTON))
    wait_for_node(name="Approve once", present=False)
    assert_database_count_stable(database, 1)


def audit_failure_denies(database, _base):
    run_sql("INSERT INTO safety_items(id) VALUES (1)")
    wait_for_node(name="Approve once", present=False, timeout=2)
    assert_database_count_stable(database, 0)


audit_failure_denies.audit_available = False


def named_parameter_binds_a_value(database, _base):
    run_sql("INSERT INTO safety_items(id) VALUES (:id)")
    wait_for_node(name=":id")
    set_text_by_name(":id", "7")
    invoke(wait_for_node(name="Run with values", role=pyatspi.ROLE_PUSH_BUTTON))
    wait_for_node(name="Run with values", present=False)
    wait_for_database_count(database, 1)
    assert database_ids(database) == [7], f"expected the bound value, found {database_ids(database)}"


named_parameter_binds_a_value.environment = "local"


def press_x11_text(text):
    for character in text:
        name = {" ": "space", "_": "underscore", "-": "minus"}.get(character, character)
        press_x11_key(name)
        time.sleep(POLL_SECONDS)


def favorites_path(base):
    return base / "config" / "tablepro" / "favorites.json"


def wait_for_favorites(base, predicate, description, timeout=WAIT_SECONDS):
    deadline = time.monotonic() + timeout
    path = favorites_path(base)
    last = None
    while time.monotonic() < deadline:
        if path.exists():
            try:
                last = json.loads(path.read_text(encoding="utf-8")).get("favorites", [])
            except json.JSONDecodeError:
                last = None
            if last is not None and predicate(last):
                return last
        time.sleep(POLL_SECONDS)
    raise AssertionError(f"{description}; favorites file held {last!r}")


def favorite_round_trips_through_open_quickly(_database, base):
    set_editor_text("SELECT 42 AS answer")
    press_x11_key("d", modifiers=["Control_L"])
    wait_for_node(name="Name")
    set_text_by_name("Name", "smoke favorite")
    invoke(wait_for_node(name="Save", role=pyatspi.ROLE_PUSH_BUTTON))

    saved = wait_for_favorites(
        base,
        lambda favorites: any(entry["name"] == "smoke favorite" for entry in favorites),
        "the favorite was not written",
    )
    entry = next(item for item in saved if item["name"] == "smoke favorite")
    assert entry["sql"] == "SELECT 42 AS answer", f"unexpected statement: {entry['sql']!r}"
    assert "last_used_at" not in entry, "a new favorite must not look used"

    press_x11_key("p", modifiers=["Control_L"])
    wait_for_node(name="smoke favorite")
    press_x11_text("smoke")
    wait_for_node(name="smoke favorite")
    press_x11_key("Return")
    wait_for_favorites(
        base,
        lambda favorites: any(item.get("last_used_at") for item in favorites),
        "opening the favorite did not record a use",
    )


favorite_round_trips_through_open_quickly.environment = "local"


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: gtk_safety.py /path/to/tablepro-app")
    binary = Path(sys.argv[1]).resolve()
    if not binary.is_file():
        raise SystemExit(f"application binary not found: {binary}")
    for scenario in [
        dismissed_approval_denies,
        approve_once_prompts_again,
        audit_failure_denies,
        named_parameter_binds_a_value,
        favorite_round_trips_through_open_quickly,
    ]:
        run_scenario(binary, scenario)
        print(f"passed: {scenario.__name__}")


if __name__ == "__main__":
    main()
