# iai-raw

The library that decodes camera RAW files for iAi. It is
[rawloader](https://github.com/pedrocr/rawloader) and
[rawler](https://github.com/dnglab/dnglab) (both LGPL-2.1) with a thin layer
over them, and it is under the LGPL too (`LGPL-2.1-or-later`; the text is in
[`licenses/LGPL-2.1.txt`](../../licenses/LGPL-2.1.txt)).

iAi has it one of two ways:

- **Linked in.** A build of this repository (`cargo build --release`) links the
  crate into `iai.exe`. That build is free software as a whole.
- **As a file of its own.** The builds published on the Releases page hold a
  part that is not open (the Pro pack). They do not link this library in: they
  load it at run time from `iai_raw.dll`, which lies beside `iai.exe`. That
  file is yours to replace.

## Putting your own build in place of `iai_raw.dll`

1. Change what you want here, or in rawloader / rawler (point `Cargo.toml` of
   this folder at your copies).
2. Build it, from the root of the repository:

   ```
   cargo build --release -p iai-raw
   ```

3. Copy `target\release\iai_raw.dll` over the `iai_raw.dll` beside `iai.exe`
   (close the app first). The app's own update replaces the file again with
   the published one; keep your copy.

The app takes any library that speaks the interface it was built with. That
interface is four C functions and one struct, described in
[`crates/iai-raw-api/src/ffi.rs`](../iai-raw-api/src/ffi.rs) (`MIT`); the
number `iai_raw_abi()` returns has to be the one the app expects, and the app
says so when it is not. A library written in another language, on other
decoders, works as well.

The exact sources of a published `iai_raw.dll` are this folder and
`crates/iai-raw-api` at the commit the release is tagged on, with the versions
of rawloader, rawler and everything under them that `Cargo.lock` pins
(crates.io keeps each of them).
