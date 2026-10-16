# Xenogears (native build)

The whole game, run from the game's own program on a software model of the console it ran on. One native executable,
no runtime to install. It plays from the data folder exported from your own disc (`GameData`, which is private and is
never part of this project).

## Run

Put `Xenogears.exe` next to the `GameData` folder (or one folder above it) and start it. Otherwise:

    Xenogears.exe --data "C:\path\to\GameData"

(or set `XENOGEARS_DATA`). Saves go to `Saves\card1.mcd` beside `GameData`; a `Saves\card2.mcd` there is the second
memory card slot.

## Controls

| Key | Pad |
| --- | --- |
| Arrow keys | Directions |
| X or Space | Cross |
| Z | Circle |
| C | Square |
| V | Triangle |
| A / S | L1 / R1 |
| Q / W | L2 / R2 |
| Return | Start |
| Tab | Select |
| Escape | Quit |

A game controller works too: A Cross, B Circle, X Square, Y Triangle, the bumpers L1/R1, the triggers L2/R2, Back
Select, Start Start, the direction pad or the left stick for the directions.

## Build

    cargo build --release

(Rust, stable; the package script in this folder builds and writes the zip.)

## Check against the reference

`Xenogears.exe --headless <frames> <every> <prefix> [schedule]` runs without a window and writes the display every N
frames as raw RGBA (`XG_WAV=file.wav` also writes the sound). The schedule is a list of `frame:button[:hold]` entries or
`mash:button:N`. The 36,100 frames from the first start to the first field of a new game match the C# reference
(`Tools/xgprog`) byte for byte, pictures and sound.

## The decompiled program

The game's code is not interpreted or translated block by block any more where it can be avoided: `tools/decomp/decomp.py`
turns every function of the main program and its overlays into structured Rust (`decomp/src`, crate `xgdec`: registers are
locals, loops and branches are real `loop` / `if` / `match`, calls are calls, constants are folded into addresses). The
program runs on a thread of its own that hands over at every vertical blank. A function that does not follow the calling
convention stays as translated blocks. `XG_DEC=0` runs the translated blocks only; `XG_DEC_CHECK=K` checks the first K calls
of every decompiled function against the translated code. Regenerate with
`python tools/decomp/decomp.py writeall decomp/src dynamic` (`XG_FOLD=0` turns the expression folding off).
Save states cannot be taken in the middle of a decompiled run.

## The graphics card

When a graphics card can be opened (wgpu: Vulkan, DirectX 12 or Metal, no window of its own), the large picture is drawn by
it instead of by the worker threads (`src/gpu/accel.rs`; `XG_GPU=0` keeps the software picture). The same primitives that the
software picture draws go to a render target of the console's video memory times the size factor as triangles and sprites that
read their texels from a copy of the program's own video memory (a 16-bit texture on the card): palettes, texture windows, the
console's four semi-transparency modes, sub-pixel vertices, perspective texturing and the texture filters are done in the shader
and the blend unit. The program's own video memory is still drawn exactly as before by the software GPU (it is what the program
reads back); what it changes is uploaded to the card when a textured primitive is going to read it. Movie frames (24-bit) keep
the cubic scaler. Against the software picture the card's differs by less than 1 on the 0-255 scale on average (edges differ
by a fraction of a pixel); a 52,700-frame battle run takes 38 s instead of 69 s. With `--res 1` the console's own picture is
used.

## The sharper picture

The window draws the picture 3 times the console's own size by default (`--res 1..4`, F2 cycles it; the size also steps
down by itself if the machine cannot hold 60 frames a second). Every drawing command is also rasterized into a second,
larger copy of video memory in full colour (`src/gpu/hi.rs`): polygons from the same vertices at finer steps with no
dithering, sprites and textures sampled between texels (`--filter 0..2`, F1 cycles: nearest, balanced, smooth; cut-out
edges never darken and a sprite never reaches into its neighbour on the texture page), movie frames smoothed up. The
game's own video memory is drawn exactly as before, so nothing the program reads back changes; with `--res 1` the output
is byte for byte the console's picture. `XG_RES` and `XG_FILTER` set the same for `--headless` (default 1, so
comparisons with the reference are unchanged).

Two passes run over the finished picture (`src/post.rs`): edge smoothing (F3, `--aa 0|1`, `XG_AA`; blends the stair-steps
left on polygon and sprite edges along the edge, only where there is contrast) and, for movie frames, a cubic scaler (block seams in movies are softened first). Projected vertices keep their sub-pixel position in the large picture, and the host resamples the sound with a cubic filter.
When the machine cannot hold 60 frames a second the size steps down first, then the edge smoothing goes, then the size.

Texel cells are interpolated along the diagonal whose ends are more alike (diagonal edges stay straight; where the two diagonals are about equally alike the texels around the cell decide, so staircases and thin lines keep their direction, and neighbouring large pixels in one cell share the lookup), and polygons the
geometry coprocessor transformed are textured in perspective in the large picture: every vertex it projects is recorded
with its screen position, depth and frame, and the draw looks the three vertices up (`XG_PERSP=0` turns it off).

### Threads

The large picture is drawn by worker threads (`src/gpu/thr.rs`; `XG_THREADS=n`, default cores - 1 up to 6, 0 draws it on
the program's own thread as before). The program's thread records the drawing commands it executes (packets, image loads,
fills, the vertex depths) and sends a batch every frame. Each worker holds a full copy of video memory and runs the same
commands, but draws the large copy only for its stripes of 16 video-memory rows, so any texture can be read and every
polygon is shared evenly. At the end of a frame the workers write their rows, smooth the edges in bands and hand the
picture over; the window shows the previous frame's picture. The result is byte for byte the single-thread picture
(checked at 1, 2 and 3 workers). The sound also gets a short prime and a fade over any gap, so a late frame cannot click.
`XG_RUNSECS=n` makes the window leave after n seconds and report the frame rate and the number of sound gaps.

### Title screen entries and speed

Three entries sit in a column to the right of New Game / Continue / Sound on the title menu (`src/gpu/title.rs`): picture
size (x1 to x4), texture filter (Sharp, Mixed, Smooth) and edges (On, Off). Right moves to them, Up / Down choose, Cross
changes the setting, Left or Circle goes back; the program sees no buttons while they have the focus. They are drawn as
extra packets in the menu's own list, with the game's marble pictures and palette and a small pixel font written into free
video memory. Choices are saved in `Saves/picture.cfg` (with F1 to F3).

The window starts at size 3 (or the saved one), steps down when the workers fall behind (their backlog, or the frame time)
and back up after a long stretch of headroom unless the size was chosen by hand. Sound plays up to 3% slower or faster as
the program runs behind or ahead instead of running dry. At size 4 on four workers the field scenes hold 60 frames a
second; the heaviest battle scenes settle at size 2 to 3.

## macOS

The program has no Windows-only code. On a Mac with Rust installed, `sh build-mac.sh` builds one file that runs on Apple Silicon and Intel;
put it beside the `GameData` and `Saves` folders. `.github/workflows/mac.yml` does the same on a hosted Mac (run it by hand from the Actions tab).
The window, sound and pad libraries are the cross-platform ones (minifb, cpal, gilrs); it has not been run on a Mac yet.

## Sorted game data

`tools/sort_gamedata.py <GameData>` moves the numbered files into named folders (`fields/`, `images/`, `compressed/`, `data/`,
`movies/`) and writes `layout.csv` (entry, archive or stream, new path, old path). The player reads `layout.csv` when it is
there and falls back to `archive/NNNN.bin` and `streams/NNNN.str` for any entry it does not list, so both layouts work.
`--undo` puts every file back. A run of 6,000 frames from the title through New Game gives identical pictures in both layouts.

`tools/unpack_fields.py <GameData>` goes one step further for the room files: each `fields/NNNN.bin` becomes a folder with
`header.bin` and one `sNN_<section>.lzss` per section (textures, walkmesh, models, sprites, palettes, scripts, unknown6,
dialogue, triggers), listed in `layout.csv` with `|` between them; the player joins them in that order, byte for byte.
The same sections are also written decompressed under `fields/NNNN/unpacked/` (textures also as separate `.tim` files) for
reading; the player does not use those. A file is split only when the parts rebuild it exactly. `--undo` joins them back.

## Native blocks (static recompilation)

`tools/recomp.py` turns the code the program actually ran into Rust (`src/machine/native/gen.rs`): the player is run with
`XG_TRACE=<file>` (headless, through the title, New Game, the movies, a battle, the field), which logs each word as it
ran, each address entered by a call, jump or taken branch, and a mark whenever the host is about to read data into
memory (an overlay may be coming). The generator cuts that code into basic blocks and writes one Rust function per block,
with the words it was made from.

At run time (`src/machine/native.rs`) a block is used only while memory holds exactly those words and no host hook sits
inside it; the interpreter stays underneath for everything else. A block does what the interpreter does for the same
instructions: same registers, same memory paths (RAM directly, devices through the machine), same step count (a block
that does not fit in the frame's remaining steps is interpreted), and it is left at the first store that reached the
devices if that made an interrupt due. `XG_NATIVE=0` turns blocks off; `XG_NATIVE_INFO=1` prints how much ran translated;
`XG_DIGEST=file[:every]` writes a hash of the registers and all of memory per frame, for comparing runs.

Checked: over the whole New Game run (36,600 frames, boot, movies, the Gear battle, Fei's room) the digests equal the
interpreter's at all 732 samples, 94.7 % of the instructions ran translated; a battle state 100 %; two random-play runs of
56,000 frames each in the field 100 %, digests equal. Code the traces never reached stays interpreted (exact, just
not translated), so more traces mean more coverage: `python tools/recomp.py trace1 trace2 ...` then build (about five
minutes, the generated file is large). A final step (calling blocks directly without the dispatcher loop) is not done.
