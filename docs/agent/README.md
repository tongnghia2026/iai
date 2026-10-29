# iAi for AI agents (MCP)

iAi is driven by AI agents (Claude Desktop, Claude Code, any MCP client) through the
**Model Context Protocol** over stdio. Everything the window does is a typed command
(`iai-engine`); an agent calls the same commands, on documents of its own or on the
window the person has open. Hướng dẫn tiếng Việt cho người dùng: [HUONG_DAN.md](HUONG_DAN.md).

## Three ways to run

| | Command | Documents |
|---|---|---|
| On its own | `iai.exe mcp --write-root D:\Photos` | its own, no window |
| On the open window | `iai.exe mcp --bridge --write-root D:\Photos` | the tabs the person sees |
| Window when open | `iai.exe mcp --prefer-window --write-root D:\Photos` | the window's while it lets AI in, else its own |

`--prefer-window` looks for the window when a task begins (`iai_task_begin`) and before a
call while nothing is open on its own documents; work under way is not moved.

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

### Antigravity: the "Nhờ AI làm" button

The person picks a job and a folder in the window and presses **Gửi cho Gemini**; Gemini,
in Antigravity, does it on the window. **Nối với Antigravity** (in that window) sets up,
under `~/.gemini/config`:

- `mcp_config.json`: the `iai` server as `iai.exe mcp --prefer-window` (the folders it had
  are kept, else `Downloads`);
- `config.json`: `mcp(iai/<tool>)` for every tool, so none asks; the relay turned on
  (`sidecars.iai.enabled`); the file as it was is kept once as `config.json.bak-iai`;
- `sidecars/iai/sidecar.json`: `iai.exe antigravity-relay`, which Antigravity runs beside
  it with what starting a conversation needs (`ANTIGRAVITY_AGENTAPI_EXE`…).

Antigravity reads its permissions and MCP servers when it starts, so it is started again.
The window turns the bridge on when an errand is sent. The relay asks the window for
errands (`next_errand`, about every 1.5 s while linked), starts a conversation for each
(`agentapi new-conversation --title=… -- <ask>`) and says how that went; the window
grants the errand's folder to read and its `iAi_ket_qua` to write (`grants`), hears the
task begin and end, counts each command as a step, and after **Dừng** refuses the agent's
calls. An errand nobody takes for 6 s starts Antigravity; one untaken in 90 s is given up.

The ask forbids Antigravity's image generator unless the person ticks **Dùng AI online cho ca
khó** (ID photos and "Việc khác"; never ID cards): then the hard cases (clothes, text over a
face, a messy backdrop) go to `invoke_subagent {TypeName: "image-generator"}`, whose pictures
land in `~/.gemini/antigravity/brain/<conversation>/`. The window grants that folder to read
for the errand, and **Nối với Antigravity** adds it as a `--read-root` of the MCP server, so
the agent opens the picture in iAi and goes on with iAi's commands.

"Sửa ảnh đang mở (AI online)" works on the tab shown, not a folder:
- the window writes the page, flattened onto white and at most 2048 px long, to
  `<photo's folder>\iAi_ket_qua\<name>_gui_AI.jpg`;
- the ask names that file, the tab's `doc` id and the generator frame nearest the photo's;
- the agent puts the drawn picture back as a new layer of that tab
  (`layer.place {doc, from: {path}, fit: "cover", name: "AI: …"}`) and exports a copy to
  `<name>_AI.jpg` beside it.

"Thay áo, lấp vai ảnh thẻ đang mở (AI online)" (Pro; also the **Thay áo, lấp vai bằng AI…**
button of the made ID photo, which applies the retouch shown first) is for a tight shot whose
shoulders the ID photo's frame cut off, or for other clothes, without the AI's redraw moving
the face:
- the window writes the photo with what the customer's photo did not reach painted grey
  (`id_photo.dress_input`);
- the agent has the generator fill the grey (or dress the person, keeping the head as it is);
- `id_photo.dress {doc, path, keepClothes}` lays the drawing back as a new masked layer,
  turned and scaled so its 478 face landmarks fall on the photo's (least squares). All above
  the chin — face, hair, backdrop — stays the photo's own pixels; below it the mask shows the
  person the AI drew (`keepClothes`: only where the photo had nothing). `faceOffPx` is how far
  the landmarks stay apart once laid; above `height / 150` the message warns that the AI drew
  the face another shape.

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
  `iai_export_pdf`, `iai_file_save`, `iai_file_list`, `iai_file_contact_sheet`,
  `iai_image_perspective_crop`, `iai_image_adjust`, `iai_image_auto_levels`,
  `iai_image_filter`, `iai_image_scan_cleanup`, `iai_layer_place`, and (Pro)
  `iai_id_photo_make`, `iai_id_photo_batch`, `iai_print_impose_sheet`, `iai_print_card_sheet`;
  the others (`edit.fill`, `edit.clear`, `select.ellipse`, `select.polygon`, `select.modify`,
  `select.color_range`, `select.wand`, `layer.transform`, `layer.duplicate`, `layer.merge_down`,
  `layer.via_copy`, `layer.flatten`, `layer.group`, `layer.merge`, `layer.clip`,
  `layer.align`, `layer.distribute`, `layer.step_copy`, `vector.shaping`, `vector.powerclip`,
  `page.delete`, `page.move`, `page.rename`, `image.mode`, `image.canvas_size`, `doc.close`, …)
  through `iai_command_run`;
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
- **Pro commands** (`id_photo.make`, `id_photo.batch`, `print.impose_sheet`,
  `print.card_sheet`) run when this machine's Pro is unlocked or on trial; otherwise they
  answer `permission_denied` saying where the person opens Pro (Help ▸ Gói Pro).
- **Long batches:** `doc.close` a photo once it is written, so a hundred photos do not stay
  open at once (a document with changes not saved needs `discard: true`).

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

## Tone, colour, filters, pages

- `image.auto_levels` — a dull, dark or tinted photo (a card or a paper shot with a phone)
  stretched end to end (`monochromatic`: Auto Contrast; `neutralMidtones`: Auto Color, a cast
  taken out of the greys too); `image.adjust {adjustment: {kind: …}}` for a set change (levels,
  curves `[[in, out], …]` 0–255, brightness_contrast, hue_saturation, vibrance, exposure,
  color_balance, invert, desaturate, threshold, posterize, black_and_white, photo_filter).
  hue_saturation takes `colorize {hue, saturation, lightness}` (one tint for the whole picture)
  and `ranges {reds | yellows | greens | cyans | blues | magentas: {hue, saturation, lightness}}`.
- `image.canvas_size {widthPx, heightPx, anchor?, fill?}` — the page larger or smaller round
  `anchor` (top_left … bottom_right, `center`), what the Background gains in `fill`.
- `image.filter {filter: {kind: …}}` — gaussian_blur, sharpen, high_pass, add_noise,
  pixelate, reduce_noise; inside the selection when there is one.
- `image.scan_cleanup` — a photographed page made a clean scan (white paper, black writing);
  `look: "bilevel"` for pure black on white.
- `layer.place {from: {path} | {doc}, box, fit}` — a picture centred on the page or in a box:
  a photo centred on A4, several photos on one page (one box each), a page of cards. With
  `text.add` and `vector.add_shape` that lays out a card or a sheet without a script.
- `layer.transform {layer, scale | scaleX/scaleY, angle, dx, dy}` — a picture layer scaled,
  turned by any angle round its middle and moved, as Free Transform does (a scan a little
  askew: duplicate the Background, then turn the copy by `angle`).
- `layer.free_transform {layers?, scaleX, scaleY, angle, dx, dy, pivot?, corners?, bounds?}` — what
  Ctrl+T does, with the same pixels: several layers together, type and shapes kept editable where
  the gesture allows, masks along, a distort or perspective by the box's four `corners`, and a
  picture transformed again resampled from its first pixels.
- Layers as the Layers panel and the Move tool do them: `layer.group {layers}` /
  `layer.ungroup`, `layer.merge {what: picked | group | merge_visible | visible}` (`merge_visible`:
  the visible layers become one, hidden ones stay; `visible`: stamped into a new one on top),
  `layer.clip` (to the layer below), `layer.align {layers, align: left | horizontal_center | right | top |
  vertical_center | bottom}`, `layer.distribute {layers, axis}`, `layer.step_copy {layers, dx,
  dy}` (a row of copies: call it again, it goes on from the last copy).
- Shapes: `vector.to_curves {layer}`, `vector.shaping {op: weld | trim | intersect | simplify,
  layers}`, `vector.powerclip {op: place, layers: [photo, frame]}` (a photo inside a shape; then
  `op: fit | fill | center | stretch` to arrange it, `extract` to take it out).
- Selections: `select.rect {rect}`, `select.ellipse {rect}` (the ellipse filling the box),
  `select.polygon {points: [[x, y], …]}`, `select.color_range {color | at, fuzziness}`,
  `select.wand {at, tolerance}` (each with `mode`: replace, add, subtract, intersect),
  `select.modify {change: feather | grow | shrink | smooth | border, amount}`; then
  `edit.fill {color}` or `edit.clear`.

## ID cards and ID photos a customer sent

A folder holds, in no order, ID card fronts and backs photographed at a slant, phone
screenshots of the electronic ID, and portraits.

For the ID cards alone, `print.card_folder {path}` does it all without looking at a photo:
each card found and laid flat upright, each front put beside its back by the ID number both
carry (read off the cards on this machine), people in the order their photos came, one
document of as many A4 pages as it takes. Cards worth a look are marked; `alone`,
`unread` and `skipped` say what it could not place (a screenshot, a portrait: add those
with `print.card_sheet`). Otherwise:

1. `file.contact_sheet {path}` — every photo small and numbered in one picture, with the
   list of what each number is: tell the fronts, backs, screenshots and portraits apart, and
   whose they are (the name and number on the card);
2. `print.card_sheet {items: [{front: {path}, back: {path}}, {screen: {path}}], copies}` —
   each card found and laid flat at 85.6 × 53.98 mm, front and back in a row, a screenshot
   turned to lie down, four rows an A4 page, one document; a Vietnamese ID card (2021 or
   2024 kind, either side) is known by its print and comes out upright by itself (its
   `kind` in the result; no `turn`); a photo whose card is not found is named in the
   error: give its `corners` (read off a preview);
3. `id_photo.batch {items: [{path, copies, name}], size, backdrop, paper}` — an ID photo of
   each portrait, all on photo paper, each person as many times as asked;
4. stop there: the person looks the pages over, fixes a card from its right-click menu
   (or `print.card_fix {layer, corners}` after `print.card_source {layer}`) and prints.
   `export.pdf {doc, path}` only when asked for a PDF.

`image.perspective_crop` lays one card (or any sheet, with `widthMm`/`heightMm`, e.g. an A4
page 210 × 297) flat in a document; follow it with `image.scan_cleanup` for a paper. The saved skills `in-can-cuoc-len-a4` and `anh-the-nhieu-nguoi` do steps 2–3.

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
