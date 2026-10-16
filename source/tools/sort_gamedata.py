# Sorts a game-data folder (archive/NNNN.bin, streams/NNNN.str) into named folders and writes layout.csv, which the player reads.
#   python sort_gamedata.py <GameData folder>          sort
#   python sort_gamedata.py <GameData folder> --undo   put every file back where it was
import os, sys, shutil

def main():
    root = sys.argv[1]
    undo = "--undo" in sys.argv
    layout = os.path.join(root, "layout.csv")
    if undo:
        if not os.path.exists(layout): print("no layout.csv: nothing to undo"); return
        for line in open(layout, encoding="utf-8"):
            parts = line.rstrip("\n").split(",", 3)
            if len(parts) != 4: continue
            entry, cls, path, orig = parts
            if "|" in path:
                print("run unpack_fields.py --undo first")
                return
            src, dst = os.path.join(root, path), os.path.join(root, orig)
            if os.path.exists(src):
                os.makedirs(os.path.dirname(dst), exist_ok=True)
                shutil.move(src, dst)
        os.remove(layout)
        for d in ("data", "fields", "images", "compressed", "movies"):
            p = os.path.join(root, d)
            if os.path.isdir(p) and not os.listdir(p): os.rmdir(p)
        print("restored")
        return
    if os.path.exists(layout): print("already sorted (layout.csv exists)"); return
    kinds = {}
    for line in open(os.path.join(root, "index.csv"), encoding="utf-8"):
        p = line.rstrip("\n").split(",", 3)
        if len(p) == 4 and p[0].isdigit(): kinds[int(p[0])] = p[3].strip()
    def folder(kind):
        k = kind.split(" ")[0].rstrip(":;")
        return {"field": ("fields", ".bin"), "tim": ("images", ".tim"), "lzss": ("compressed", ".bin"), "raw": ("data", ".bin")}.get(k, ("data", ".bin"))
    rows = []
    for name in sorted(os.listdir(os.path.join(root, "archive"))):
        if not name.endswith(".bin"): continue
        n = int(name[:-4])
        d, ext = folder(kinds.get(n, "raw"))
        rows.append((n, "archive", f"{d}/{n:04d}{ext}", f"archive/{name}"))
    for name in sorted(os.listdir(os.path.join(root, "streams"))):
        if not name.endswith(".str"): continue
        n = int(name[:-4])
        rows.append((n, "stream", f"movies/{n:04d}.str", f"streams/{name}"))
    for n, cls, path, orig in rows:
        src, dst = os.path.join(root, orig), os.path.join(root, path)
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        shutil.move(src, dst)
    with open(layout, "w", encoding="utf-8", newline="\n") as f:
        for r in rows: f.write(",".join(str(x) for x in r) + "\n")
    for d in ("archive", "streams"):
        p = os.path.join(root, d)
        if os.path.isdir(p) and not os.listdir(p): os.rmdir(p)
    print(f"sorted {len(rows)} files")

if __name__ == "__main__":
    main()
