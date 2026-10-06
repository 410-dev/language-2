# Differential fuzzer for language-2 (run from anywhere; uses target/debug/language-2).
import os, random, subprocess, glob, sys, tempfile
root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
exe = os.path.join(root, "target", "debug", "language-2" + (".exe" if os.name == "nt" else ""))
work = os.path.join(tempfile.gettempdir(), "l2arith")
os.makedirs(work, exist_ok=True)
seed = int(sys.argv[1]) if len(sys.argv) > 1 else 1
rounds = int(sys.argv[2]) if len(sys.argv) > 2 else 10
random.seed(seed)

INTS = {"Int8": (-128, 127), "Int16": (-32768, 32767), "Int32": (-2**31, 2**31 - 1), "Int64": (-2**63, 2**63 - 1),
        "UInt8": (0, 255), "UInt16": (0, 65535), "UInt32": (0, 2**32 - 1), "UInt64": (0, 2**64 - 1)}
FLOATS = ["Float32", "Float64", "Float16"]

def lit(t):
    lo, hi = INTS[t]
    r = random.random()
    if r < 0.3:
        v = random.choice([lo, hi, 0, 1, -1 if lo < 0 else 2, lo + 1, hi - 1])
    elif r < 0.6:
        v = random.randint(max(lo, -300), min(hi, 300))
    else:
        v = random.randint(lo, hi)
    return v

def gen_program(policy):
    lines = ["@using sdk 1", "@compiler(EnableSoftwareEmulation=true)", "@runtimecfg(IntegerOverflow=%s)" % policy, "using stdio as stdio", "", "function void main() {"]
    n = 0
    for t in INTS:
        for _ in range(12):
            a, b = lit(t), lit(t)
            op = random.choice(["+", "-", "*", "/", "%", "&&", "||", "^", "<<", ">>", "**", "==", "<", ">=", "neg", "~", "cast"])
            n += 1
            lines.append("    try {")
            lines.append("        %s a%d = %d" % (t, n, a))
            lines.append("        %s b%d = %d" % (t, n, b))
            if op == "neg":
                if t.startswith("U"):
                    expr = "~a%d" % n
                else:
                    expr = "-a%d" % n
            elif op == "~":
                expr = "~a%d" % n
            elif op == "cast":
                target = random.choice(list(INTS) + ["Float64", "Float32"])
                expr = "a%d.castTo(%s)" % (n, target)
            elif op == "**":
                lines.append("        %s e%d = %d" % (t, n, random.randint(0, 9)))
                expr = "a%d ** e%d" % (n, n)
            elif op in ("<<", ">>"):
                lines.append("        Int32 s%d = %d" % (n, random.randint(0, 70)))
                expr = "a%d %s s%d" % (n, op, n)
            else:
                expr = "a%d %s b%d" % (n, op, n)
            lines.append('        stdio.println(f"%d {%s}")' % (n, expr))
            if op in ("+", "-", "*"):
                w = {"+": "addWrap", "-": "subWrap", "*": "mulWrap"}[op]
                lines.append('        stdio.println(f"%dw {a%d.%s(b%d)}")' % (n, n, w, n))
            lines.append("    } catch (ArithmeticException e) {")
            lines.append('        stdio.println(f"%d ! {e.getMessage()}")' % n)
            lines.append("    }")
    for t in FLOATS:
        for _ in range(8):
            a = random.choice([0.0, -0.0, 1.5, -2.25, 1e10, 3.14159, 0.1, 65504.0, 1e-5, 123456.789])
            b = random.choice([0.0, 2.0, -3.5, 0.1, 7.0, 1e-3])
            op = random.choice(["+", "-", "*", "/", "%", "**", "<", "=="])
            n += 1
            lines.append("    %s fa%d = %r" % (t, n, a))
            lines.append("    %s fb%d = %r" % (t, n, b))
            lines.append('    stdio.println(f"%d {fa%d %s fb%d}")' % (n, n, op, n))
            lines.append('    stdio.println(f"%dc {fa%d.castTo(Int64)}")' % (n, n) if abs(a) < 1e9 else "")
    lines.append("}")
    return "\n".join(lines) + "\n"

mismatch = 0
for r in range(rounds):
    for policy in ("error", "wrap"):
        path = os.path.join(work, "arith_%d_%d_%s.l2" % (seed, r, policy))
        open(path, "w").write(gen_program(policy))
        outs = {}
        for be in ("interpreter", "bytecode", "compiler"):
            p = subprocess.run([exe, "run", "-q", "-b", be, path], capture_output=True, timeout=120)
            outs[be] = (p.returncode, p.stdout.replace(b"\r\n", b"\n"), p.stderr[-300:])
        if len(set((v[0], v[1]) for v in outs.values())) > 1:
            mismatch += 1
            ref = outs["interpreter"][1].split(b"\n")
            for be in ("bytecode", "compiler"):
                o = outs[be][1].split(b"\n")
                for i, (x, y) in enumerate(zip(ref, o)):
                    if x != y:
                        print("MISMATCH", path, be, "line", i, ":", x, "vs", y)
                        break
                else:
                    if len(ref) != len(o) or outs[be][0] != outs["interpreter"][0]:
                        print("MISMATCH", path, be, "exit", outs[be][0], outs[be][2])
print("rounds:", rounds, "mismatching programs:", mismatch)
