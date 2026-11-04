"""Writes the output of the `calibrate_styles` test into src/styles.rs.

    cargo test --release --lib calibrate -- --ignored --nocapture | python3 tools/apply_calibration.py

Clean Studio is the plugin's default state, so its values go into the parameter defaults in
src/params.rs instead.
"""
import re
import sys

rows = [l.strip().split("|") for l in sys.stdin if l.startswith("CAL|")]
styles = open("src/styles.rs").read()
params = open("src/params.rs").read()

def fmt(v):
    v = float(v)
    return str(int(v)) if v == int(v) else repr(round(v, 3))

for _, name, thr, amt, trim, *_ in rows:
    values = {"fet_thr": thr, "opt_amt": amt, "trim": trim}
    if name == "Clean Studio":
        for pat, val in [
            (r'(thresh: db\("Punch threshold", )([-\d.]+)', thr),
            (r'(amount: pct\("Level amount", )([-\d.]+)', amt),
            (r'(trim: db\("Style trim", )([-\d.]+)', trim),
        ]:
            params, n = re.subn(pat, lambda m: m.group(1) + fmt(val) if "." in fmt(val) else m.group(1) + fmt(val) + ".0", params)
            assert n == 1, pat
        continue
    start = styles.index(f'name: "{name}",')
    open_at = styles.index("values: v! {", start) + len("values: v! {")
    close_at = styles.index("\n        },", open_at)
    block = styles[open_at:close_at]
    for key, val in values.items():
        new, n = re.subn(rf'"{key}" = [-\d.]+', f'"{key}" = {fmt(val)}', block)
        if n == 0:
            new = block.rstrip() + f' "{key}" = {fmt(val)},'
            if not block.strip():
                new = f'\n            "{key}" = {fmt(val)},'
        block = new
    styles = styles[:open_at] + block + styles[close_at:]

open("src/styles.rs", "w").write(styles)
open("src/params.rs", "w").write(params)
print(f"applied {len(rows)} styles")
