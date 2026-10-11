Xenogears - native build. One program, no runtime to install.

Run Xenogears.exe. The game data is in the GameData folder beside it.
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
