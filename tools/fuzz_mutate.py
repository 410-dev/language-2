# Differential fuzzer for language-2 (run from anywhere; uses target/debug/language-2).
import os, random, subprocess, glob, sys, tempfile
root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
exe = os.path.join(root, "target", "debug", "language-2" + (".exe" if os.name == "nt" else ""))
work = os.path.join(tempfile.gettempdir(), "l2fuzz")
os.makedirs(work, exist_ok=True)
random.seed(int(sys.argv[1]) if len(sys.argv) > 1 else 1)
runs = int(sys.argv[2]) if len(sys.argv) > 2 else 300
native = len(sys.argv) > 3 and sys.argv[3] == "native"
files = glob.glob(os.path.join(root, "tests", "programs", "*.l2"))
tokens = ["(", ")", "{", "}", "[", "]", ",", "=", "+", "*", "&", "?", "!", "null", "x", "Int64", "String",
          "return", "this", "\n", "f\"{", "\"", "->", "move", "super", "case", ":", "1", "0.5", "<", ">>",
          "-", "2", "0", "Int8", "UInt8", "% 3", "* 1000000", "** 2", ".clone()", "break", "continue"]
NL = "\n"
panics = 0
valid = 0
mismatches = 0
for i in range(runs):
    src = open(random.choice(files), encoding="utf-8").read()
    chars = list(src)
    for _ in range(random.randint(1, 3)):
        op = random.random()
        pos = random.randrange(len(chars))
        if op < 0.35:
            del chars[pos:pos + random.randint(1, 6)]
        elif op < 0.85:
            chars[pos:pos] = list(random.choice(tokens))
        else:
            lines = "".join(chars).split(NL)
            a, b = random.randrange(len(lines)), random.randrange(len(lines))
            lines[a], lines[b] = lines[b], lines[a]
            chars = list(NL.join(lines))
    path = os.path.join(work, "case%d.l2" % i)
    open(path, "w", encoding="utf-8").write("".join(chars))
    crashed = False
    for cmd in (["check", "-q", path], ["emit-llvm", "-q", path, "-o", path + ".ll"], ["disasm", "-q", path]):
        r = subprocess.run([exe] + cmd, capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=60)
        if r.returncode == 101 or "panicked" in r.stderr:
            panics += 1
            crashed = True
            print("PANIC", cmd[0], path)
            print(NL.join(l for l in r.stderr.splitlines() if "panicked" in l)[:600])
            break
    if crashed:
        continue
    r = subprocess.run([exe, "check", "-q", path], capture_output=True)
    if r.returncode != 0:
        continue
    valid += 1
    outs = {}
    backends = ["interpreter", "bytecode"] + (["compiler"] if native else [])
    for be in backends:
        try:
            rr = subprocess.run([exe, "run", "-q", "-b", be, path], capture_output=True, timeout=30,
                                input=b"x\n1\n", env=dict(os.environ, L2_SEED="3"))
            err_last = rr.stderr.replace(b"\r\n", b"\n").strip().split(b"\n")[-1:]
            outs[be] = (rr.returncode, rr.stdout.replace(b"\r\n", b"\n"), tuple(err_last))
        except subprocess.TimeoutExpired:
            outs[be] = ("timeout", b"", ())
    if len(set(outs.values())) > 1:
        mismatches += 1
        print("MISMATCH", path)
        for k, v in outs.items():
            print("   ", k, v[0], v[1][-300:], v[2])
print("panics:", panics, "valid:", valid, "mismatches:", mismatches, "of", runs)
