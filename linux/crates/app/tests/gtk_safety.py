#!/usr/bin/env python3
import ctypes
import csv
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
CONNECTION_B_NAME = "Safety SQLite B"
BROKEN_CONNECTION_NAME = "Broken SQLite"
WAIT_SECONDS = 15
POLL_SECONDS = 0.05
FILE_CHOOSER_ROLES = (pyatspi.ROLE_FILE_CHOOSER, pyatspi.ROLE_DIALOG)
SETTLE_SECONDS = 3.0


def descendants(node):
    yield node
    try:
        children = list(node)
    except Exception:
        return
    for child in children:
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


def role_matches(node, role):
    if role is None:
        return True
    if isinstance(role, tuple):
        return node_role(node) in role
    return node_role(node) == role


def application_node():
    desktop = pyatspi.Registry.getDesktop(0)
    for application in desktop:
        names = {node_name(application)}
        names.update(node_name(node) for node in descendants(application))
        if APP_NAME in names or any(CONNECTION_NAME in name for name in names):
            return application
    return None


def desktop_applications():
    return list(pyatspi.Registry.getDesktop(0))


def find_node(name=None, role=None):
    for application in desktop_applications():
        for node in descendants(application):
            if name is not None and node_name(node) != name:
                continue
            if not role_matches(node, role):
                continue
            return node
    return None


def find_within(root, name=None, role=None):
    for node in descendants(root):
        if name is not None and node_name(node) != name:
            continue
        if not role_matches(node, role):
            continue
        return node
    return None


def wait_within(root, name=None, role=None, present=True, timeout=WAIT_SECONDS):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        node = find_within(root, name=name, role=role)
        if (node is not None) == present:
            return node
        time.sleep(POLL_SECONDS)
    expectation = "appear" if present else "disappear"
    raise AssertionError(
        f"expected {name!r} to {expectation} inside {node_name(root)!r}:\n{accessible_snapshot()}"
    )


def invoke_named_action_within_node(root, anchor_name, action_name):
    anchor = wait_within(root, name=anchor_name, role=pyatspi.ROLE_LIST_ITEM)
    for node in descendants(anchor):
        if node_name(node) == action_name:
            invoke(node)
            return
    raise AssertionError(
        f"no action {action_name!r} within {anchor_name!r}:\n{accessible_snapshot()}"
    )


def set_editor_text_within(root, sql):
    candidates = []
    for node in descendants(root):
        if node_role(node) != pyatspi.ROLE_TEXT:
            continue
        try:
            editable = node.queryEditableText()
            candidates.append((node, editable))
        except Exception:
            continue
    if not candidates:
        raise AssertionError(f"no SQL editor inside {node_name(root)!r}:\n{accessible_snapshot()}")
    node, editable = max(candidates, key=lambda candidate: candidate[0].queryText().characterCount)
    editable.setTextContents(sql)
    return node


def run_sql_within(root, sql):
    set_editor_text_within(root, sql)
    invoke(wait_within(root, name="Run", role=pyatspi.ROLE_PUSH_BUTTON))


def find_node_containing(text, role=None):
    for application in desktop_applications():
        for node in descendants(application):
            if not role_matches(node, role):
                continue
            if text in node_name(node):
                return node
    return None


def wait_for_node_containing(text, role=None, present=True, timeout=WAIT_SECONDS):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        node = find_node_containing(text, role=role)
        if (node is not None) == present:
            return node
        time.sleep(POLL_SECONDS)
    expectation = "appear" if present else "disappear"
    raise AssertionError(
        f"expected an accessible name containing {text!r} to {expectation}:\n{accessible_snapshot()}"
    )


def find_frame_containing(text):
    for application in desktop_applications():
        for node in descendants(application):
            if node_role(node) != pyatspi.ROLE_FRAME:
                continue
            if text in node_name(node):
                return node
    return None


def wait_for_frame_containing(text, present=True, timeout=WAIT_SECONDS):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        node = find_frame_containing(text)
        if (node is not None) == present:
            return node
        time.sleep(POLL_SECONDS)
    expectation = "appear" if present else "disappear"
    raise AssertionError(
        f"expected a window title containing {text!r} to {expectation}:\n{accessible_snapshot()}"
    )


def accessible_snapshot():
    applications = desktop_applications()
    if not applications:
        return "application unavailable"
    entries = []
    for application in applications:
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
                if action_name in {"click", "activate", "press", "toggle", "default.activate"} and action.doAction(index):
                    return
        except Exception:
            continue
    raise AssertionError(f"no invokable action for {node_name(node)!r}")


def invoke_named_action_within(anchor_name, action_name):
    anchor = wait_for_node(name=anchor_name, role=pyatspi.ROLE_LIST_ITEM)
    for node in descendants(anchor):
        if node_name(node) != action_name:
            continue
        invoke(node)
        return
    raise AssertionError(
        f"no action {action_name!r} within {anchor_name!r}:\n{accessible_snapshot()}"
    )


def invoke_accessible_action(action_name):
    applications = desktop_applications()
    if not applications:
        raise AssertionError("application unavailable")
    for application in applications:
        for node in descendants(application):
            try:
                actions = node.queryAction()
                for index in range(actions.nActions):
                    if actions.getName(index) == action_name and actions.doAction(index):
                        return
            except Exception:
                continue
    raise AssertionError(f"no accessible action {action_name!r}:\n{accessible_snapshot()}")


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
        for application in desktop_applications():
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


def set_visible_editable_within(anchor_name, anchor_role, text):
    anchor = wait_for_node(name=anchor_name, role=anchor_role)
    for node in descendants(anchor):
        try:
            extents = node.queryComponent().getExtents(pyatspi.DESKTOP_COORDS)
            if extents.width <= 1 or extents.height <= 1:
                continue
            editable = node.queryEditableText()
            editable.setTextContents(text)
            return
        except Exception:
            continue
    raise AssertionError(f"no visible editable control within {anchor_name!r}:\n{accessible_snapshot()}")


def database_ids(path):
    with sqlite3.connect(path) as connection:
        return [row[0] for row in connection.execute("SELECT id FROM safety_items ORDER BY id")]


def database_tables(path):
    with sqlite3.connect(path) as connection:
        return {
            row[0]
            for row in connection.execute(
                "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name"
            )
        }


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
    database_b = base / "safety-b.sqlite"
    # `note` is the only editable column: `id` is the primary key, which
    # inline editing refuses. Scenarios that only count or list ids are
    # unaffected because it is nullable.
    with sqlite3.connect(database) as connection:
        connection.execute("CREATE TABLE safety_items (id INTEGER PRIMARY KEY, note TEXT)")
    with sqlite3.connect(database_b) as connection:
        connection.execute("CREATE TABLE safety_items (id INTEGER PRIMARY KEY, note TEXT)")

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
            },
            {
                "id": "66a8b513-d771-4fe7-8ee0-ec1099254cb1",
                "name": CONNECTION_B_NAME,
                "driver_id": "sqlite",
                "host": "",
                "port": 0,
                "database": str(database_b),
                "username": "",
                "use_tls": False,
                "tls_mode": "disabled",
                "read_only": False,
                "auth_mode": "password",
                "environment": environment,
            },
            {
                "id": "6ff0268f-84cc-44f5-aeb4-a5d7609fb0fc",
                "name": BROKEN_CONNECTION_NAME,
                "driver_id": "sqlite",
                "host": "",
                "port": 0,
                "database": str(base / "missing" / "broken.sqlite"),
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
    (tablepro_config / "preferences.json").write_text(
        json.dumps(
            {
                "default_page_size": 100,
                "confirm_destructive": True,
                "editor_font_size": 12,
                "history_retention_days": 30,
                "query_timeout_secs": 60,
            }
        ),
        encoding="utf-8",
    )

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
    invoke_named_action_within(CONNECTION_NAME, "Open connection")
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
        artifact_dir = None
        if failure is not None and os.environ.get("TABLEPRO_GTK_ARTIFACT_DIR"):
            artifact_dir = Path(os.environ["TABLEPRO_GTK_ARTIFACT_DIR"])
            artifact_dir.mkdir(parents=True, exist_ok=True)
            (artifact_dir / f"{scenario.__name__}-accessibility.txt").write_text(
                accessible_snapshot(), encoding="utf-8"
            )
            if shutil.which("scrot"):
                subprocess.run(
                    ["scrot", str(artifact_dir / f"{scenario.__name__}.png")],
                    check=False,
                )
        if process is not None:
            stderr = stop_application(process)
        if artifact_dir is not None:
            (artifact_dir / f"{scenario.__name__}-stderr.txt").write_text(stderr, encoding="utf-8")
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
    invoke_named_action_within("smoke favorite", "Open smoke favorite")
    wait_for_favorites(
        base,
        lambda favorites: any(item.get("last_used_at") for item in favorites),
        "opening the favorite did not record a use",
    )


favorite_round_trips_through_open_quickly.environment = "local"


def open_saved_connection(name):
    invoke(wait_for_node(name="Open saved connection", role=pyatspi.ROLE_TOGGLE_BUTTON))
    invoke_named_action_within(name, "Open connection")


def successful_switch_keeps_database_ownership(database, base):
    run_sql("CREATE TABLE ownership_old (id INTEGER PRIMARY KEY); INSERT INTO safety_items(id) VALUES (11)")
    wait_for_database_count(database, 1)

    open_saved_connection(CONNECTION_B_NAME)
    wait_for_node(name=f"{CONNECTION_B_NAME} — TablePro", role=pyatspi.ROLE_FRAME)
    invoke(wait_for_node(name="Open SQL editor"))
    wait_for_node(name="Run", role=pyatspi.ROLE_PUSH_BUTTON)
    run_sql("CREATE TABLE ownership_new (id INTEGER PRIMARY KEY); INSERT INTO safety_items(id) VALUES (22)")

    database_b = base / "safety-b.sqlite"
    wait_for_database_count(database_b, 1)
    assert database_ids(database) == [11], f"old database changed after switch: {database_ids(database)}"
    assert database_ids(database_b) == [22], f"new database did not receive the write: {database_ids(database_b)}"
    assert "ownership_old" in database_tables(database)
    assert "ownership_new" not in database_tables(database)
    assert "ownership_new" in database_tables(database_b)
    assert "ownership_old" not in database_tables(database_b)


successful_switch_keeps_database_ownership.environment = "local"


def failed_switch_preserves_the_old_workspace(database, _base):
    open_saved_connection(BROKEN_CONNECTION_NAME)
    wait_for_node(name="Connection failed")
    invoke(wait_for_node(name="Close", role=pyatspi.ROLE_PUSH_BUTTON))
    wait_for_node(name="Connection failed", present=False)

    # The original editor remains attached to the original connection.
    run_sql("INSERT INTO safety_items(id) VALUES (33)")
    wait_for_database_count(database, 1)
    assert database_ids(database) == [33]


failed_switch_preserves_the_old_workspace.environment = "local"


def running_query_is_cancelled_before_switch(database, base):
    run_sql(
        "WITH RECURSIVE counter(value) AS "
        "(VALUES(0) UNION ALL SELECT value + 1 FROM counter WHERE value < 1000000000) "
        "SELECT sum(value) FROM counter"
    )
    wait_for_node(name="Cancel", role=pyatspi.ROLE_PUSH_BUTTON)
    open_saved_connection(CONNECTION_B_NAME)
    invoke(wait_for_node(name="Cancel queries and switch", role=pyatspi.ROLE_PUSH_BUTTON))

    wait_for_node(name=f"{CONNECTION_B_NAME} — TablePro", role=pyatspi.ROLE_FRAME)
    invoke(wait_for_node(name="Open SQL editor"))
    wait_for_node(name="Run", role=pyatspi.ROLE_PUSH_BUTTON)
    run_sql("INSERT INTO safety_items(id) VALUES (44)")
    database_b = base / "safety-b.sqlite"
    wait_for_database_count(database_b, 1)
    assert database_ids(database) == [], "the cancelled old query must not mutate its database"
    assert database_ids(database_b) == [44]


running_query_is_cancelled_before_switch.environment = "local"


def current_page_csv_export_is_pk_ordered(database, base):
    # The notes carry every character CSV has to quote: a separator, a
    # quote, and a line break. A page export that got the escaping wrong
    # would come back as a different number of rows or columns.
    awkward = {
        1: "a,b",
        2: 'say "hi"',
        3: "line1\nline2",
        4: "trailing backslash \\",
    }
    with sqlite3.connect(database) as connection:
        connection.executemany(
            "INSERT INTO safety_items(id, note) VALUES (?, ?)",
            ((identifier, awkward.get(identifier)) for identifier in range(150, 0, -1)),
        )

    invoke_named_action_within("safety_items", "Open safety_items")
    # The total comes from COUNT(*). SQLite used to decode that column as
    # NULL, so the label silently dropped the "of 150" half; asserting the
    # full label keeps that from regressing.
    wait_for_node(name="Rows 1 – 100 of 150")
    invoke_accessible_action("win.export-csv")
    wait_for_node(name="Export current page as CSV", role=FILE_CHOOSER_ROLES)
    set_visible_editable_within(
        "Export current page as CSV",
        FILE_CHOOSER_ROLES,
        str(base / "home" / "current-page.csv"),
    )
    invoke(wait_for_node(name="Save", role=pyatspi.ROLE_PUSH_BUTTON))

    export = base / "home" / "current-page.csv"
    deadline = time.monotonic() + WAIT_SECONDS
    while time.monotonic() < deadline and not export.exists():
        time.sleep(POLL_SECONDS)
    if not export.exists():
        raise AssertionError(f"current-page export was not written:\n{accessible_snapshot()}")
    with export.open(newline="", encoding="utf-8") as handle:
        rows = list(csv.reader(handle))
    assert rows[0] == ["id", "note"], f"unexpected CSV header: {rows[0]!r}"
    exported_ids = [int(row[0]) for row in rows[1:]]
    assert exported_ids == list(range(1, 101)), (
        f"expected the first PK-ordered page only, found {len(exported_ids)} rows: "
        f"{exported_ids[:5]}…{exported_ids[-5:]}"
    )
    exported_notes = {int(row[0]): row[1] for row in rows[1:]}
    for identifier, note in awkward.items():
        assert exported_notes[identifier] == note, (
            f"row {identifier} lost its text through the CSV export: "
            f"{exported_notes[identifier]!r} != {note!r}"
        )


current_page_csv_export_is_pk_ordered.environment = "local"


def pending_edits_gate_a_connection_switch(database, base):
    invoke_named_action_within("safety_items", "Open safety_items")
    wait_for_node(name="No rows on this page")
    invoke(wait_for_node(name="Insert row", role=pyatspi.ROLE_PUSH_BUTTON))
    wait_for_node(name="1 unsaved change")

    open_saved_connection(CONNECTION_B_NAME)
    wait_for_node(name="Save changes before switching?")
    invoke(wait_for_node(name="Stay", role=pyatspi.ROLE_PUSH_BUTTON))
    wait_for_node(name="Save changes before switching?", present=False)
    wait_for_frame_containing(f"{CONNECTION_NAME} — TablePro")
    wait_for_frame_containing(f"{CONNECTION_B_NAME} — TablePro", present=False)
    wait_for_node(name="1 unsaved change")
    assert_database_count_stable(database, 0)

    open_saved_connection(CONNECTION_B_NAME)
    wait_for_node(name="Save changes before switching?")
    invoke(wait_for_node(name="Discard and switch", role=pyatspi.ROLE_PUSH_BUTTON))
    wait_for_frame_containing(f"{CONNECTION_B_NAME} — TablePro")
    wait_for_node(name="1 unsaved change", present=False)
    assert database_ids(database) == [], (
        f"a discarded pending row reached the old database: {database_ids(database)}"
    )

    invoke(wait_for_node(name="Open SQL editor"))
    wait_for_node(name="Run", role=pyatspi.ROLE_PUSH_BUTTON)
    run_sql("INSERT INTO safety_items(id) VALUES (55)")
    database_b = base / "safety-b.sqlite"
    wait_for_database_count(database_b, 1)
    assert database_ids(database_b) == [55]
    assert database_ids(database) == [], "the old connection stayed writable after the switch"


pending_edits_gate_a_connection_switch.environment = "local"


def a_browse_tab_reads_the_new_connection_after_a_switch(database, base):
    database_b = base / "safety-b.sqlite"
    with sqlite3.connect(database) as connection:
        connection.executemany(
            "INSERT INTO safety_items(id) VALUES (?)",
            ((identifier,) for identifier in range(1, 8)),
        )
    with sqlite3.connect(database_b) as connection:
        connection.executemany(
            "INSERT INTO safety_items(id) VALUES (?)",
            ((identifier,) for identifier in range(1, 4)),
        )

    invoke_named_action_within("safety_items", "Open safety_items")
    wait_for_node_containing("Rows 1 – 7")

    open_saved_connection(CONNECTION_B_NAME)
    wait_for_frame_containing(f"{CONNECTION_B_NAME} — TablePro")
    wait_for_node_containing("Rows 1 – 7", present=False)

    invoke_named_action_within("safety_items", "Open safety_items")
    wait_for_node_containing("Rows 1 – 3")
    assert database_ids(database) == list(range(1, 8)), "the old database changed after the switch"
    assert database_ids(database_b) == [1, 2, 3]


a_browse_tab_reads_the_new_connection_after_a_switch.environment = "local"


def two_windows_hold_two_connections(database, base):
    database_b = base / "safety-b.sqlite"

    invoke_accessible_action("win.new-window")
    second_window = wait_for_node(name="TablePro", role=pyatspi.ROLE_FRAME)
    invoke_named_action_within_node(second_window, CONNECTION_B_NAME, "Open connection")
    second_window = wait_for_frame_containing(f"{CONNECTION_B_NAME} — TablePro")
    invoke(wait_within(second_window, name="Open SQL editor"))
    wait_within(second_window, name="Run", role=pyatspi.ROLE_PUSH_BUTTON)

    first_window = find_frame_containing(f"{CONNECTION_NAME} — TablePro")
    assert first_window is not None, (
        f"the first window lost its connection when the second one connected:\n{accessible_snapshot()}"
    )

    run_sql_within(second_window, "INSERT INTO safety_items(id) VALUES (77)")
    wait_for_database_count(database_b, 1)
    assert database_ids(database) == [], "the second window wrote to the first window's database"

    run_sql_within(first_window, "INSERT INTO safety_items(id) VALUES (88)")
    wait_for_database_count(database, 1)

    assert database_ids(database) == [88], f"first window database: {database_ids(database)}"
    assert database_ids(database_b) == [77], f"second window database: {database_ids(database_b)}"


two_windows_hold_two_connections.environment = "local"


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: gtk_safety.py /path/to/tablepro-app")
    binary = Path(sys.argv[1]).resolve()
    if not binary.is_file():
        raise SystemExit(f"application binary not found: {binary}")
    scenarios = [
        dismissed_approval_denies,
        approve_once_prompts_again,
        audit_failure_denies,
        named_parameter_binds_a_value,
        favorite_round_trips_through_open_quickly,
        successful_switch_keeps_database_ownership,
        failed_switch_preserves_the_old_workspace,
        running_query_is_cancelled_before_switch,
        current_page_csv_export_is_pk_ordered,
        pending_edits_gate_a_connection_switch,
        a_browse_tab_reads_the_new_connection_after_a_switch,
        two_windows_hold_two_connections,
    ]
    selected = os.environ.get("TABLEPRO_GTK_SCENARIO")
    if selected:
        scenarios = [scenario for scenario in scenarios if scenario.__name__ == selected]
        if not scenarios:
            raise SystemExit(f"unknown GTK scenario: {selected}")
    for scenario in scenarios:
        run_scenario(binary, scenario)
        print(f"passed: {scenario.__name__}")


if __name__ == "__main__":
    main()
