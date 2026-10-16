Xenogears - native build. One program, no runtime to install.

GAME DATA (not included)
This package contains the player only. It does not contain the game's data, which you take off your own
discs once. You need your own copy of both discs of the game (the North American release, serials SLUS-00664
and SLUS-00669; the Japanese serials SLPS-01160 and SLPS-01161 are read as well).

1. Make a raw image of each disc: a .bin file with 2352 bytes to the sector (a .bin/.cue pair from any
   disc-imaging tool; a plain .iso has no movie sectors and will not give you the movies).
2. Put Xenogears.exe in its own folder and run, from a command prompt in that folder:

       Xenogears.exe --extract "Disc 1.bin" "Disc 2.bin"

   This writes a GameData folder beside the program (about 1.2 GB) holding the files of both discs as one
   data set. A different folder can be chosen with --out <folder>. It takes a few minutes. Disc 2 is
   optional (without it the second half's movies are missing).
3. Run Xenogears.exe. The game finds GameData beside it.

Saves are kept in the Saves folder (card1.mcd; a card2.mcd there is the second memory card slot).

Keys
  Arrow keys     Directions
  X or Space     Cross
  Z              Circle
  C              Square
  V              Triangle
  A / S          L1 / R1
  Q / W          L2 / R2
  Return         Start
  Tab            Select
  Esc            Quit
  Alt+Enter      Full screen and back (XG_FULLSCREEN=1 starts in full screen)

Controller
  A Cross, B Circle, X Square, Y Triangle, bumpers L1 / R1, triggers L2 / R2,
  Back Select, Start Start, D-pad or left stick for the directions.

Picture
  The picture is drawn larger than the game's own 320 x 240 (3 times by default): polygons and sprites are
  rendered at the larger size in full colour, with texture filtering and no dithering. Models keep the sub-pixel
  position of their points (no one-pixel jitter), movie block seams are softened, and the sound is resampled with a
  cubic filter.
  F1   cycles the texture filter: nearest, balanced (default), smooth
  F2   cycles the picture size 1 to 4 times (1 is the original picture)
  F3   switches the edge smoothing on and off (movies are always scaled with a cubic filter)
  If the machine cannot hold 60 frames a second, the size steps down by itself, then the edge smoothing.
  The larger picture is drawn by your graphics card (Vulkan or DirectX 12) when one is available, so even 4 times is
  smooth; if no card can be opened it is drawn on several processor cores instead (all but one, at most six).
  Set the environment variable XG_GPU=0 to force the processor drawing.
  From a command line: Xenogears.exe --res 2 --filter 1 --aa 1

On the title screen three more entries sit to the right of New Game, Continue and Sound: size (x1 to x4),
texture filter (Sharp, Mixed, Smooth) and edges (On, Off). Press Right to move to them, Up / Down to choose, Cross to
change the setting, Left or Circle to go back. The choices are kept in Saves\picture.cfg.

Starting a new game: at the title menu choose New Game with the arrows and Cross.

Future versions would include graphical upgrades (IE a remaster) and then integrating the lost sections of the second disc. However, I'm fully willing to let someone else do that as well.


Changes since the last upload
  Native code: the game's own program is now translated to native code (3,783 functions, each checked against the
  interpreter), and the main memory areas run natively; far fewer interpreted instructions per frame.
  Window program: no console box, a program icon and version information, statically linked (nothing to install),
  saves beside the game or in %APPDATA%\Xenogears\Saves, key remapping in the settings screen (F12), and Alt+Enter for
  full screen and back (XG_FULLSCREEN=1 starts in full screen).
  Widescreen: battle, world map and arena scenes can be drawn in real 16:9 (F6) instead of stretched; the interface keeps
  its size in the middle. Everything else is unchanged.
  Picture: FSR upscaling, frame blending, CRT look and colour options in the settings screen; texture filtering and
  movie scaling improved.
  Textures: pages can be replaced by 8x versions placed in TextureReplace (loaded in the background, so no stalls).
  Movies: a faster built-in picture decoder; replacement movie packs are shown when present.
  Stability: the program writes Saves\crash.state, Saves\game-log.txt and freeze-report.txt when it ends unexpectedly;
  long random-play runs (24 of 70,000 frames) all finished cleanly.
  Setup: install.ps1 installs the player for the current user (optional data, Start-menu shortcut, uninstall.ps1);
  package.ps1 builds a ready-to-run folder and a source zip.
  Source: the player's source is in the source folder. A macOS build is made by the "macOS build" action (Actions tab,
  Run workflow); it produces one file that runs on Apple Silicon and Intel, and is not yet tested on a Mac.
