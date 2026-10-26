#!/usr/bin/env python3
"""Generate site.wat for the demo guest.

The pages are real HTML held as static data in linear memory. Writing the WAT
by hand would mean hand-counting byte offsets for every string, which breaks
silently the moment a page is edited; deriving them here keeps the offsets
correct by construction.
"""
import pathlib

HERE = pathlib.Path(__file__).parent


def page(title, heading, body):
    """A complete HTML document sharing one stylesheet across the site."""
    return f"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
:root {{ color-scheme: light dark; --bg:#fbfaf8; --fg:#1c1b19; --muted:#6b6862;
        --line:#e5e1da; --accent:#4f46e5; --card:#ffffff; }}
@media (prefers-color-scheme: dark) {{
  :root {{ --bg:#16151a; --fg:#eceaf2; --muted:#a09dab; --line:#2c2a33;
           --accent:#a5a0ff; --card:#1e1d24; }}
}}
* {{ box-sizing: border-box; }}
body {{ margin:0; background:var(--bg); color:var(--fg);
       font:16px/1.65 ui-sans-serif, -apple-system, "Segoe UI", system-ui, sans-serif; }}
.wrap {{ max-width:44rem; margin:0 auto; padding:3.5rem 1.5rem 4rem; }}
header {{ border-bottom:1px solid var(--line); padding-bottom:1.5rem; margin-bottom:2rem; }}
h1 {{ font-size:2rem; line-height:1.2; margin:0 0 .4rem; letter-spacing:-.02em; }}
.sub {{ color:var(--muted); margin:0; font-size:.95rem; }}
nav {{ display:flex; gap:.5rem; flex-wrap:wrap; margin-top:1.5rem; }}
nav a {{ color:var(--fg); text-decoration:none; border:1px solid var(--line);
        padding:.4rem .85rem; border-radius:999px; font-size:.875rem; background:var(--card); }}
nav a:hover {{ border-color:var(--accent); color:var(--accent); }}
.card {{ background:var(--card); border:1px solid var(--line); border-radius:12px;
        padding:1.25rem 1.4rem; margin:1rem 0; }}
.card h2 {{ margin:0 0 .5rem; font-size:1.05rem; }}
.card p {{ margin:0; color:var(--muted); font-size:.925rem; }}
code {{ font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size:.875em;
       background:var(--bg); border:1px solid var(--line); border-radius:5px; padding:.1em .4em; }}
footer {{ margin-top:2.5rem; padding-top:1.25rem; border-top:1px solid var(--line);
         color:var(--muted); font-size:.85rem; }}
.tag {{ display:inline-block; font-size:.75rem; color:var(--accent);
       border:1px solid var(--accent); border-radius:999px; padding:.1rem .6rem; }}
</style>
</head>
<body>
<div class="wrap">
<header>
<span class="tag">clean-server</span>
<h1>{heading}</h1>
<p class="sub">Served from a WebAssembly component through the clean:host contract.</p>
<nav>
<a href="/">Home</a>
<a href="/about">About</a>
<a href="/posts/hello-world">A post</a>
<a href="/missing">404</a>
</nav>
</header>
{body}
<footer>clean-server &middot; wasmtime component model &middot; no framework in the request path</footer>
</div>
</body>
</html>
"""


HOME = page(
    "clean-server &mdash; demo site",
    "It serves real pages.",
    """<div class="card">
<h2>This is HTML, not a fixture string</h2>
<p>The guest is a WebAssembly component. It registered these routes at startup
through <code>clean:host/routing</code> and wrote this document with
<code>clean:host/response</code>. The host never parsed a template.</p>
</div>
<div class="card">
<h2>Routing with parameters</h2>
<p>Visit <code>/posts/hello-world</code> &mdash; the slug is captured by the host,
handed to the guest via <code>get-param</code>, and rendered into the page below
the title.</p>
</div>
<div class="card">
<h2>Status codes are the guest's decision</h2>
<p>An unmatched path returns a real 404 with its own styled page, because the
guest sets the status itself rather than letting the host guess.</p>
</div>""",
)

ABOUT = page(
    "About &mdash; clean-server",
    "About this host.",
    """<div class="card">
<h2>What clean-server owns</h2>
<p>The HTTP surface only: listener, request parsing, response writing, TLS,
WebSocket upgrade, and SSE. Composition, pooling, and WASI belong to
<code>clean-host-core</code>.</p>
</div>
<div class="card">
<h2>The contract is a file</h2>
<p><code>host.wit</code> is the authoritative declaration of what this host
provides. CI fails when the wasmtime linker and that file disagree, so a guest
can trust what it imports.</p>
</div>
<div class="card">
<h2>Instances are pooled and reset</h2>
<p>Each request checks out an instance, runs, and is reset before returning to
the pool &mdash; so one request can never observe another's leftover state.</p>
</div>""",
)


def post(slug_placeholder):
    return page(
        "A post &mdash; clean-server",
        "A routed post.",
        """<div class="card">
<h2>Slug captured from the URL</h2>
<p>The host matched <code>/posts/:slug</code> and passed the value below to the
guest. Change the URL and this line changes with it.</p>
</div>
<div class="card">
<h2>slug</h2>
<p><code>SLUG_GOES_HERE</code></p>
</div>""",
    )


NOTFOUND = page(
    "404 &mdash; clean-server",
    "No route for that path.",
    """<div class="card">
<h2>404</h2>
<p>The guest registered no handler for this path, so it set status 404 and
rendered this page. The host did not invent an error document.</p>
</div>""",
)

# --- layout -----------------------------------------------------------------
# Every string the module references lives at a computed offset. Offsets are
# 8-byte aligned purely so the generated WAT stays readable.
BLOBS = []
cursor = 0


def blob(raw):
    global cursor
    off = cursor
    BLOBS.append((off, raw))
    cursor = (off + len(raw) + 8) & ~7
    return off, len(raw)


def s(text):
    return blob(text.encode())


P_ROOT = s("/")
P_ABOUT = s("/about")
P_POST = s("/posts/:slug")
P_WILD = s("/*rest")
K_SLUG = s("slug")
H_CTYPE = s("content-type")
V_HTML = s("text/html; charset=utf-8")
LOG_MSG = s("served the home page")
LOG_KEY = s("page")
LOG_VAL = s("demo-site")

B_HOME = s(HOME)
B_ABOUT = s(ABOUT)
B_404 = s(NOTFOUND)

# The post page is emitted in two halves so the captured slug can be written
# between them at request time.
POST_FULL = post(None)
head_text, tail_text = POST_FULL.split("SLUG_GOES_HERE")
B_POST_HEAD = s(head_text)
B_POST_TAIL = s(tail_text)

# Scratch space above the static data: the composed post body, then the
# canonical-ABI return areas.
STATIC_END = (cursor + 15) & ~15
SCRATCH = STATIC_END
SCRATCH_SIZE = 65536
RET = SCRATCH + SCRATCH_SIZE
RET2 = RET + 64
# Where the log field list is laid out at call time: list<field> lowers to a
# (ptr, count) pair over contiguous (key-ptr, key-len, val-ptr, val-len) tuples,
# so the tuple itself has to live in memory rather than in the call arguments.
FIELDS = RET2 + 64
HEAP = FIELDS + 64
PAGES = (HEAP + 65536 * 2) // 65536 + 1


def data_section():
    out = []
    for off, raw in BLOBS:
        esc = "".join(
            c if 32 <= b < 127 and c not in '"\\' else f"\\{b:02x}"
            for b, c in ((b, chr(b)) for b in raw)
        )
        out.append(f'  (data (i32.const {off}) "{esc}")')
    return "\n".join(out)


WAT = f""";; testing/demo-site — a guest that serves real HTML pages.
;;
;; GENERATED BY gen.py. Edit the pages there, not here: every string offset in
;; this file is computed from the page contents, so hand-editing the WAT would
;; desynchronise the offsets from the data section.
;;
;; It exists because testing/fake-guest is a protocol fixture — it returns
;; plain-text bodies chosen to make assertions cheap. This guest instead proves
;; the same contract carries a real site: full HTML documents, a captured path
;; parameter rendered into the page, and a guest-chosen 404.
;;
;; ROUTES
;;   GET /             handler 0 -> 200 text/html, the home page
;;   GET /about        handler 1 -> 200 text/html, a second page
;;   GET /posts/:slug  handler 2 -> 200 text/html, with :slug rendered in
;;   GET /*rest        handler 3 -> 404 text/html, the guest's own error page

(module
  (import "clean:host/routing@0.1.0" "register"
    (func $register (param i32 i32 i32 i32 i32)))
  (import "clean:host/request@0.1.0" "get-param"
    (func $get-param (param i32 i32 i32)))
  (import "clean:host/response@0.1.0" "set-status"
    (func $set-status (param i32)))
  (import "clean:host/response@0.1.0" "add-header"
    (func $add-header (param i32 i32 i32 i32)))
  (import "clean:host/response@0.1.0" "set-body"
    (func $set-body (param i32 i32)))
  (import "clean:host/log@0.1.0" "emit"
    (func $log-emit (param i32 i32 i32 i32 i32)))

  (memory (export "memory") {PAGES})

{data_section()}

  (global $RET  i32 (i32.const {RET}))
  (global $RET2 i32 (i32.const {RET2}))
  (global $SCRATCH i32 (i32.const {SCRATCH}))

  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32)
    ;; Nothing host-allocated outlives a call here, so a fixed bump above the
    ;; scratch and return areas satisfies the ABI.
    (i32.const {HEAP}))

  (func $html-headers
    (call $add-header
      (i32.const {H_CTYPE[0]}) (i32.const {H_CTYPE[1]})
      (i32.const {V_HTML[0]}) (i32.const {V_HTML[1]})))

  ;; Copy `len` bytes from `src` to `dst`, returning the end of the written run.
  (func $copy (param $dst i32) (param $src i32) (param $len i32) (result i32)
    (memory.copy (local.get $dst) (local.get $src) (local.get $len))
    (i32.add (local.get $dst) (local.get $len)))

  (func (export "init")
    ;; 0 = get. Trailing 1 = CSRF on, which is the default for page routes.
    (call $register (i32.const 0) (i32.const {P_ROOT[0]})  (i32.const {P_ROOT[1]})  (i32.const 0) (i32.const 1))
    (call $register (i32.const 0) (i32.const {P_ABOUT[0]}) (i32.const {P_ABOUT[1]}) (i32.const 1) (i32.const 1))
    (call $register (i32.const 0) (i32.const {P_POST[0]})  (i32.const {P_POST[1]})  (i32.const 2) (i32.const 1))
    ;; A trailing wildcard, so an unmatched path reaches the guest's 404 page
    ;; rather than the host's built-in one.
    (call $register (i32.const 0) (i32.const {P_WILD[0]})  (i32.const {P_WILD[1]})  (i32.const 3) (i32.const 1)))

  (func (export "handle") (param $h i32)
    (local $ptr i32)
    (local $len i32)
    (local $end i32)

    (call $set-status (i32.const 200))
    (call $html-headers)

    ;; GET /
    (if (i32.eq (local.get $h) (i32.const 0))
      (then
        ;; One field: (key "page", value "demo-site"). A list<record> lowers to
        ;; (ptr, count) over contiguous (ptr,len,ptr,len) tuples, so build the
        ;; tuple in memory first and pass its address.
        (i32.store (i32.const {FIELDS})    (i32.const {LOG_KEY[0]}))
        (i32.store (i32.const {FIELDS + 4}) (i32.const {LOG_KEY[1]}))
        (i32.store (i32.const {FIELDS + 8}) (i32.const {LOG_VAL[0]}))
        (i32.store (i32.const {FIELDS + 12}) (i32.const {LOG_VAL[1]}))
        (call $log-emit
          (i32.const 2)                 ;; level: info
          (i32.const {LOG_MSG[0]}) (i32.const {LOG_MSG[1]})
          (i32.const {FIELDS}) (i32.const 1))
        (call $set-body (i32.const {B_HOME[0]}) (i32.const {B_HOME[1]}))
        (return)))

    ;; GET /about
    (if (i32.eq (local.get $h) (i32.const 1))
      (then
        (call $set-body (i32.const {B_ABOUT[0]}) (i32.const {B_ABOUT[1]}))
        (return)))

    ;; GET /posts/:slug — splice the captured slug between the two halves.
    (if (i32.eq (local.get $h) (i32.const 2))
      (then
        (local.set $end
          (call $copy (global.get $SCRATCH)
                      (i32.const {B_POST_HEAD[0]}) (i32.const {B_POST_HEAD[1]})))
        (call $get-param (i32.const {K_SLUG[0]}) (i32.const {K_SLUG[1]}) (global.get $RET))
        ;; option<string>: discriminant at +0, then (ptr, len) at +4, +8.
        (if (i32.eq (i32.load (global.get $RET)) (i32.const 1))
          (then
            (local.set $ptr (i32.load offset=4 (global.get $RET)))
            (local.set $len (i32.load offset=8 (global.get $RET)))
            (local.set $end
              (call $copy (local.get $end) (local.get $ptr) (local.get $len)))))
        (local.set $end
          (call $copy (local.get $end)
                      (i32.const {B_POST_TAIL[0]}) (i32.const {B_POST_TAIL[1]})))
        (call $set-body (global.get $SCRATCH)
                        (i32.sub (local.get $end) (global.get $SCRATCH)))
        (return)))

    ;; GET /*rest — the guest's own 404.
    (call $set-status (i32.const 404))
    (call $set-body (i32.const {B_404[0]}) (i32.const {B_404[1]})))
)
"""

(HERE / "site.wat").write_text(WAT)
print(f"wrote site.wat  static={STATIC_END}B scratch={SCRATCH_SIZE}B pages={PAGES}")
