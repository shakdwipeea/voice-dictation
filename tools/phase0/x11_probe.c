#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/extensions/XTest.h>
#include <X11/keysym.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static int fake_key(Display *display, KeySym keysym, int needs_shift) {
    KeyCode code = XKeysymToKeycode(display, keysym);
    KeyCode shift = XKeysymToKeycode(display, XK_Shift_L);
    if (code == 0) {
        return 0;
    }
    if (needs_shift) {
        XTestFakeKeyEvent(display, shift, True, CurrentTime);
    }
    XTestFakeKeyEvent(display, code, True, CurrentTime);
    XTestFakeKeyEvent(display, code, False, CurrentTime);
    if (needs_shift) {
        XTestFakeKeyEvent(display, shift, False, CurrentTime);
    }
    return 1;
}

static int char_to_keysym(char value, KeySym *keysym, int *needs_shift) {
    *needs_shift = 0;
    if (value >= 'a' && value <= 'z') {
        *keysym = XK_a + (value - 'a');
        return 1;
    }
    if (value >= 'A' && value <= 'Z') {
        *keysym = XK_a + (value - 'A');
        *needs_shift = 1;
        return 1;
    }
    if (value >= '0' && value <= '9') {
        *keysym = XK_0 + (value - '0');
        return 1;
    }
    switch (value) {
    case ' ': *keysym = XK_space; return 1;
    case '\n': *keysym = XK_Return; return 1;
    case '.': *keysym = XK_period; return 1;
    case ',': *keysym = XK_comma; return 1;
    case '-': *keysym = XK_minus; return 1;
    case '_': *keysym = XK_minus; *needs_shift = 1; return 1;
    case '\'': *keysym = XK_apostrophe; return 1;
    case '!': *keysym = XK_1; *needs_shift = 1; return 1;
    case ':': *keysym = XK_semicolon; *needs_shift = 1; return 1;
    default: return 0;
    }
}

static int type_text(Display *display, const char *text) {
    for (const char *cursor = text; *cursor != '\0'; cursor++) {
        KeySym keysym;
        int needs_shift;
        if (!char_to_keysym(*cursor, &keysym, &needs_shift)) {
            fprintf(stderr, "Unsupported character for Phase 0 probe: 0x%02x\n",
                    (unsigned char)*cursor);
            return 0;
        }
        if (!fake_key(display, keysym, needs_shift)) {
            fprintf(stderr, "No X11 keycode for character: %c\n", *cursor);
            return 0;
        }
    }
    XFlush(display);
    return 1;
}

static int check_extensions(Display *display) {
    int event_base;
    int error_base;
    int major;
    int minor;
    if (!XTestQueryExtension(display, &event_base, &error_base, &major, &minor)) {
        fprintf(stderr, "XTEST extension is unavailable\n");
        return 0;
    }
    printf("X11 display connected; XTEST %d.%d is available\n", major, minor);
    return 1;
}

static int wait_for_hotkey(Display *display, const char *text, int timeout_seconds) {
    Window root = DefaultRootWindow(display);
    KeyCode hotkey = XKeysymToKeycode(display, XK_F8);
    if (hotkey == 0) {
        fprintf(stderr, "Cannot resolve F8 keycode\n");
        return 0;
    }
    XGrabKey(display, hotkey, ControlMask, root, False, GrabModeAsync, GrabModeAsync);
    XSelectInput(display, root, KeyPressMask);
    XSync(display, False);

    time_t deadline = time(NULL) + timeout_seconds;
    printf("Press Ctrl+F8 within %d seconds to inject the probe text\n",
           timeout_seconds);
    fflush(stdout);
    while (time(NULL) < deadline) {
        while (XPending(display) > 0) {
            XEvent event;
            XNextEvent(display, &event);
            if (event.type == KeyPress && event.xkey.keycode == hotkey &&
                (event.xkey.state & ControlMask) != 0) {
                XUngrabKey(display, hotkey, ControlMask, root);
                XSync(display, False);
                usleep(50000);
                /* The user is still physically holding Ctrl; release it
                 * logically first or every typed key becomes a Ctrl chord. */
                XTestFakeKeyEvent(display, XKeysymToKeycode(display, XK_Control_L),
                                  False, CurrentTime);
                XTestFakeKeyEvent(display, XKeysymToKeycode(display, XK_Control_R),
                                  False, CurrentTime);
                XFlush(display);
                return type_text(display, text);
            }
        }
        usleep(10000);
    }
    XUngrabKey(display, hotkey, ControlMask, root);
    XSync(display, False);
    fprintf(stderr, "Timed out waiting for Ctrl+F8\n");
    return 0;
}

static int selftest(Display *display) {
    const char *expected = "sunoto phase zero";
    char received[128] = {0};
    size_t received_length = 0;
    Window previous_focus;
    int previous_revert;
    XGetInputFocus(display, &previous_focus, &previous_revert);

    Window root = DefaultRootWindow(display);
    Window window = XCreateSimpleWindow(display, root, 0, 0, 240, 60, 0, 0, 0);
    XStoreName(display, window, "Sunoto Phase 0 X11 Self-test");
    XSelectInput(display, window, KeyPressMask | StructureNotifyMask);
    XMapRaised(display, window);

    int mapped = 0;
    time_t map_deadline = time(NULL) + 2;
    while (time(NULL) < map_deadline && !mapped) {
        while (XPending(display) > 0) {
            XEvent event;
            XNextEvent(display, &event);
            if (event.type == MapNotify && event.xmap.window == window) {
                mapped = 1;
            }
        }
        usleep(10000);
    }
    if (!mapped) {
        fprintf(stderr, "Timed out waiting for X11 self-test window to map\n");
        XDestroyWindow(display, window);
        return 0;
    }
    XSetInputFocus(display, window, RevertToParent, CurrentTime);
    XSync(display, False);

    if (!type_text(display, expected)) {
        XDestroyWindow(display, window);
        return 0;
    }

    time_t deadline = time(NULL) + 2;
    while (time(NULL) < deadline && received_length < strlen(expected)) {
        while (XPending(display) > 0) {
            XEvent event;
            XNextEvent(display, &event);
            if (event.type == KeyPress) {
                char text[16];
                int length = XLookupString(&event.xkey, text, sizeof(text), NULL, NULL);
                if (length > 0 && received_length + (size_t)length < sizeof(received)) {
                    memcpy(received + received_length, text, (size_t)length);
                    received_length += (size_t)length;
                    received[received_length] = '\0';
                }
            }
        }
        usleep(10000);
    }

    XWindowAttributes previous_attributes;
    if (previous_focus != None && previous_focus != PointerRoot &&
        XGetWindowAttributes(display, previous_focus, &previous_attributes) != 0 &&
        previous_attributes.map_state == IsViewable) {
        XSetInputFocus(display, previous_focus, previous_revert, CurrentTime);
    }
    XDestroyWindow(display, window);
    XSync(display, False);
    if (strcmp(received, expected) != 0) {
        fprintf(stderr, "X11 injection self-test mismatch: expected '%s', got '%s'\n",
                expected, received);
        return 0;
    }
    printf("X11 injection self-test passed: %s\n", received);
    return 1;
}

static void usage(const char *program) {
    fprintf(stderr,
            "Usage:\n"
            "  %s check\n"
            "  %s selftest\n"
            "  %s insert TEXT\n"
            "  %s hotkey [TEXT] [TIMEOUT_SECONDS]\n",
            program, program, program, program);
}

int main(int argc, char **argv) {
    if (argc < 2) {
        usage(argv[0]);
        return 2;
    }
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) {
        fprintf(stderr, "Cannot connect to X11 display; DISPLAY=%s\n",
                getenv("DISPLAY") != NULL ? getenv("DISPLAY") : "");
        return 1;
    }
    if (!check_extensions(display)) {
        XCloseDisplay(display);
        return 1;
    }

    int success = 0;
    if (strcmp(argv[1], "check") == 0) {
        success = 1;
    } else if (strcmp(argv[1], "selftest") == 0) {
        success = selftest(display);
    } else if (strcmp(argv[1], "insert") == 0 && argc == 3) {
        success = type_text(display, argv[2]);
    } else if (strcmp(argv[1], "hotkey") == 0) {
        const char *text = argc >= 3 ? argv[2] : "sunoto phase zero";
        int timeout = argc >= 4 ? atoi(argv[3]) : 30;
        success = wait_for_hotkey(display, text, timeout);
    } else {
        usage(argv[0]);
        XCloseDisplay(display);
        return 2;
    }
    XCloseDisplay(display);
    return success ? 0 : 1;
}
