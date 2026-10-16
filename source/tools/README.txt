Probe scripts used to check the build against the C# reference and to walk the game by warps:
  chain.sh R1 R2 ...  warp through the rooms in order (each from the previous room's running state) and report any stop
  hop.sh P R          the same for one pair
They expect a saved machine (ng36000.rs.state: PROG_SAVE=36000:file on the new-game run) and the game data folder.
