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

The extracting is the program's own: it reads the disc's file index (sector 24) and its section table
(sector 40), writes every entry to GameData/archive/NNNN.bin and the movies and sound to GameData/streams/NNNN.str
as raw sectors, and puts the disc's own files (SYSTEM.CNF, the program file) in GameData/system/. The player
also understands a sorted GameData (layout.csv); a numbered one works as it is.

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
  The picture is drawn on several processor cores (all but one, at most six); the sound stays steady even at 3 or 4 times.
  From a command line: Xenogears.exe --res 2 --filter 1 --aa 1

On the title screen three more entries sit to the right of New Game, Continue and Sound: size (x1 to x4),
texture filter (Sharp, Mixed, Smooth) and edges (On, Off). Press Right to move to them, Up / Down to choose, Cross to
change the setting, Left or Circle to go back. The choices are kept in Saves\picture.cfg.

Starting a new game: at the title menu choose New Game with the arrows and Cross.

Future versions would include graphical upgrades (IE a remaster) and then integrating the lost sections of the second disc. However, I'm fully willing to let someone else do that as well.
