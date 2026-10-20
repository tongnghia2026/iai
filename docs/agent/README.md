# iAi for AI agents (MCP)

iAi is driven by AI agents (Claude Desktop, Claude Code, any MCP client) through the
**Model Context Protocol** over stdio. Everything the window does is a typed command
(`iai-engine`); an agent calls the same commands, on documents of its own or on the
window the person has open. Hướng dẫn tiếng Việt cho người dùng: [HUONG_DAN.md](HUONG_DAN.md).

## Two ways to run

| | Command | Documents |
|---|---|---|
| On its own | `iai.exe mcp --write-root D:\Photos` | its own, no window |
| On the open window | `iai.exe mcp --bridge --write-root D:\Photos` | the tabs the person sees |

The bridge needs the person to turn it on first: **Help ▸ Cho AI điều khiển iAi ▸ Bật**.
The window then listens on 127.0.0.1 and writes the port and a fresh 256-bit token to
`%APPDATA%\IAI\control.json`; `--bridge` reads it (or `--control-file FILE`). Turning it
off (or closing iAi) deletes the file.

Options:

- `--read-root DIR` — a folder files may be read from (repeat for more);
- `--write-root DIR` — a folder files may be written to (and read from);
- `--allow-print` — commands may print (paper and ink); off by default;
- `--allow-network` — commands may use the network (model downloads); off by default.

Files are read and written only inside the granted folders: a relative `path` is joined to
the first one, an absolute one must lie inside one. `..`, device names (`CON`, `NUL`…),
`\\?\` / `\\.\` paths and links leading out are refused.

### Claude Desktop

`%APPDATA%\Claude\claude_desktop_config.json` (Settings ▸ Developer ▸ Edit Config); merge
into an existing `mcpServers`:

```json
{
  "mcpServers": {
    "iai": {
      "command": "C:\\iAi\\iai.exe",
      "args": ["mcp", "--bridge", "--write-root", "D:\\Photos"]
    }
  }
}
```

The dialog in iAi shows this text with the real path of `iai.exe`, ready to copy.

### Claude Code

```bash
claude mcp add iai -- "C:\iAi\iai.exe" mcp --write-root "D:\Photos"
```

### Scripts, no agent

`iai.exe run script.json [same options]` runs a list of calls and prints the replies
(exit code 1 when one failed). Every call is checked before any runs. An `atomic` script is
all or nothing, so at most 256 calls; one that is not may hold up to 100 000, run 256 at a
time on the same documents, and stops at the first failure (its `index` counts from the
script's first call). An MCP batch (`iai_command_batch`) holds at most 256 calls.

```json
{"atomic": false, "calls": [
  {"command": "file.open", "args": {"path": "khach_1.jpg"}},
  {"command": "id_photo.make", "args": {"size": "3x4", "backdrop": "blue"}},
  {"command": "file.export", "args": {"path": "the/khach_1.png"}}
]}
```

## Tools

- one tool per featured command, named `iai_<command with _>`: `iai_file_open`,
  `iai_doc_inspect`, `iai_image_crop`, `iai_image_resize`, `iai_ai_remove_background`,
  `iai_ai_inpaint`, `iai_text_add`, `iai_vector_add_shape`, `iai_analyze_region`,
  `iai_export_pdf`, `iai_file_save`, and (Pro) `iai_id_photo_make`,
  `iai_print_impose_sheet`;
- `iai_command_list` (optionally by `prefix`, with each command's JSON Schema),
  `iai_command_run`, `iai_command_batch` (atomic by default: all or nothing);
- `iai_job_status` / `iai_job_cancel` for long commands started with `background: true`
  (others wait up to 110 s, then answer with a job id);
- `iai_render_preview` — a PNG of a page, to look at.

Tools whose effects were not granted (files without a folder, printing, network) are not
listed. Resources: `iai://session`, `iai://commands`, `iai://doc/{id}`.

## Working well

- **Check by numbers.** `analyze.region` gives a rectangle's mean colour (`#RRGGBB`), spread
  and coverage; `doc.inspect` gives layers with their bounds. Look at a preview only when
  numbers cannot say it.
- **Errors carry a `code`** (`bad_params`, `not_found`, `permission_denied`, `busy`,
  `not_enabled`, …) and often a `hint` with the call that fixes them.
- **Every change is one undo step**; in the window, the person sees each step and can
  `Ctrl+Z` it. `expectRevision` refuses a call when the document changed meanwhile;
  `requestId` makes a retried call answer as the first did; `dryRun` tries and takes back.
- **The person comes first.** On the open window, while the person is in the middle of an
  edit (a transform, a text being typed, a dialog with a preview, Develop), changes are
  refused with `busy` — queries still answer. Ask again a little later.
- **Pro commands** (`id_photo.make`, `print.impose_sheet`) run when this machine's Pro is
  unlocked or on trial; otherwise `permission_denied`.

## An errand, start to end

Twenty portraits in `D:\Photos\khach` → 3×4 ID photos on blue → 10×15 sheets → PDF:

1. for each photo: `file.open {path}` → `id_photo.make {doc, size: "3x4", backdrop: "blue"}`
   → `file.export {doc, path: "the/NN.png"}`;
2. `print.impose_sheet {source: {kind: "folder", path: "the"}, sheet: {kind: "grid",
   paper: "10x15", photo: "3x4"}, copies: 2}` — one new document per sheet;
3. check: for each print layer (`doc.inspect`, raster layers inside groups), the top-left
   corner's `analyze.region` mean is `#0090FF`;
4. `export.pdf {doc, path: "to_in_1.pdf"}` per sheet.

The test `agent::e2e` runs this through MCP (a light one in the suite; the full one with
`IAI_AGENT_E2E_PHOTOS=<folder of portraits>`, ignored by default).

## A many-page PDF

`file.open` renders a PDF's pages one at a time into one document, a page per PDF page:

- `pages` — which ones, 0 being the first (`[0, 1, 2]`); every page when left out. They open
  in the PDF's order, each once, so page `i` of the document is the `i`-th smallest asked.
- `dpi` — the resolution, 18 to 600; 300 when left out. A page that would pass 50 MP, or
  30 000 px a side, comes out lower; the reply's `dpi` says what it got.
- All the pages stay in memory, about 35 MB per A4 page at 300 dpi. Pages that would take
  more than an eighth of this machine's memory together (at least 200 MP: 23 A4 pages at
  300 dpi) are refused with `too_large` before any is rendered; the message gives how many
  of the first pages fit and the dpi at which all of them would, and the `hint` is the call
  that opens the first pages that fit.
- The document is a picture of the PDF, not the PDF: it has no file of its own, so
  `file.save` needs a `path` (.iai keeps the pages) and the PDF is never overwritten.
  `export.pdf` writes it back, every page or `pages`; shapes, paths and text added stay vector.

Forty A4 colouring cards (2480 × 3508 px at 300 dpi), a line of grass at the foot of each:

```json
{"atomic": false, "calls": [
  {"command": "file.open", "args": {"path": "thiep.pdf"}},
  {"command": "page.select", "args": {"page": 0}},
  {"command": "vector.add_path", "args": {"runs": [{"points": [[0, 3400], [600, 3300],
    [1240, 3380], [1900, 3290], [2480, 3400]], "smooth": true}], "stroke": "#000000",
    "strokeWidth": 8, "join": "round", "cap": "round", "name": "Cỏ"}},
  {"command": "export.pdf", "args": {"path": "thiep_co.pdf"}}
]}
```

with the `page.select` + `vector.add_path` pair repeated for pages 1 to 39 before the
export. `vector.add_path` draws
straight segments, or with `"smooth": true` a curve through the points; `join` is `miter`
(default: a sharp corner reaches out up to 4 stroke widths), `round` or `bevel`; `cap` is
`butt` (default), `round` or `square`.

## Saved skills

iAi keeps the tasks agents finish well as **skills**, so the next agent — any model — runs a
saved one instead of working the steps out again (fewer tokens, the same result every time).
The library is a folder, `Documents\iAi_Ky_nang` unless `--skills DIR` says otherwise: one
folder per skill with `kich_ban.json` (the recipe), `SKILL.md` (written from it, in the Agent
Skills layout), `lich_su.md` (each use) and, for a tier-2 skill, its programs in `helpers/`.

The flow, also in the server's instructions:

1. `iai_task_begin {request}` with what the person asked, in their words. It answers with the
   saved skills that fit best (name, what it does, its params). From here iAi records the
   calls of the task.
2. A skill fits: `iai_skill_run {name, params}` (params left out take their defaults; folders
   must be granted). `dryRun: true` checks params and commands and says what the loops will
   take. A run longer than one tool call answers `{"status": "running", "run": id}`: call
   `iai_skill_run {run: id}` until it is `done`. A tier-2 skill answers `next`: the lines to run
   with your shell. `iai_skill_get {name}` gives its `SKILL.md`; `recipe: true` its steps.
3. No skill fits: work step by step as usual and check the result.
4. `iai_task_end {status: "done", params: {...}, title, description}`. Name in `params` the
   values from the request that a like request would change (`{"size": "3x4", "copies": 2}`).
   iAi then keeps the task by itself:
   - ids a call used that an earlier call answered become references to that answer;
   - the folders read and written become params (`vao`, `ra`; a single file outside a loop
     `tep_vao`, `tep_ra`), and so do the values named;
   - calls repeated for each file of a folder, each item of an answer (`sheets`) or a number
     of rounds (pages) become one loop, when what changes between rounds is the round's file,
     item or number;
   - the same job again only counts a use; the same job with other literal values gains them
     as params; a job close to a kept one is saved as its version; anything else is new.
   With `status` other than `done` nothing is kept. A task never begun or ended is kept, as
   *unconfirmed*, when the session ends, only if every call in it worked.

`iai_skill_search {query}` finds skills by words (Vietnamese marks optional); an empty query
lists them all. Without an agent: `iai.exe run --skill NAME --param NAME=VALUE ... [--dry-run]`
with the usual `--read-root` / `--write-root`.

A recipe (`kich_ban.json`) is plain JSON; steps are commands (`command` + `args`), loops
(`foreach` over `files` of a folder with `ext` / `names`, `items` of an array, or a `count`,
`as` naming the item) or other skills (`skill` + `params`). Templates: `${name}` from the params
and the loops (`${f.path}`, `${f.name}`, `${f.stem}`, `${it.doc}`, `${i}` from 0, `${n}` from 1);
`{"$ref": "s1", "path": "/doc"}` reads step `s1`'s latest answer.
