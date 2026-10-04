# /// script
# dependencies = ["numpy", "scipy", "pillow", "numba"]
# ///
"""Photorealistic sand preview for theta-rho patterns (.thr) of kinetic sand tables.

Simulates the sand height field while the steel ball follows the path
(plough model + angle of repose) and lights it like the LED strip around the rim.

Usage: uv run sandsim.py pattern.thr [-o image.png] [--view top|oblique] [--res 0.5]
"""
import argparse
import time
from pathlib import Path

import numpy as np
from numba import njit, prange
from PIL import Image
from scipy import ndimage

# --- Table --------------------------------------------------------------------
RHO_MM = 300.0      # rho = 1 corresponds to 300 mm (ball centre)
SAND_MM = 310.0     # radius of the sand surface
BALL_R = 5.0        # ball radius (10 mm diameter)

# --- calibrated sand parameters ------------------------------------------------
P = dict(
    h0=4.5,          # sand depth [mm], the ball rolls on the floor
    repose=32.0,     # angle of repose after sliding [°]
    stat=40.0,       # steepest stable slope (freshly cut walls) [°]
    smooth=0.25,     # rounding of edges [mm]
    push_w=4.0,      # width of the deposition ring around the ball [mm]
    side=0.6,        # share deposited sideways (instead of only in front)
    clear_pitch=6.5, # ring pitch of the clearing pattern [mm]
    step=0.5,        # step size of the ball [mm]
    lag=4.0,         # lag of the ball behind the magnet [mm] (rounds corners)
)


# --- Path ----------------------------------------------------------------------
def read_thr(path):
    rows = []
    for line in Path(path).read_text().splitlines():
        line = line.split("#")[0].split()
        if len(line) >= 2:
            rows.append((float(line[0]), float(line[1])))
    return np.array(rows)


def clear_path(start_out, pitch_mm):
    """Clearing spiral like clear_from_out/in (Archimedean, linear in theta/rho)."""
    turns = RHO_MM / pitch_mm
    t = np.linspace(0, 1, int(turns * 200) + 1)
    rho = 1 - t if start_out else t
    return np.column_stack([t * turns * 2 * np.pi, rho])


def resample(tr, step_mm):
    """Subdivides linearly in theta/rho (as the table moves), step ≈ step_mm."""
    a, b = tr[:-1], tr[1:]
    d = np.hypot((b[:, 0] - a[:, 0]) * np.maximum(a[:, 1], b[:, 1]), b[:, 1] - a[:, 1])
    n = np.maximum(1, np.ceil(d * RHO_MM / step_mm)).astype(int)
    idx = np.repeat(np.arange(len(n)), n)
    f = (np.arange(n.sum()) - np.repeat(np.cumsum(n) - n, n)) / np.repeat(n, n)
    out = a[idx] + (b - a)[idx] * f[:, None]
    return np.vstack([out, tr[-1:]])


def to_xy(tr, mirror=False):
    th, rho = tr[:, 0], np.clip(tr[:, 1], 0, 1)
    y = rho * np.sin(th)
    return np.column_stack([rho * np.cos(th), -y if mirror else y]) * RHO_MM


@njit(cache=True)
def _towed(xy, lag):
    """The ball is towed by the magnet and cuts corners (tractrix-like pursuit curve)."""
    out = xy.copy()
    bx, by = xy[0, 0], xy[0, 1]
    for k in range(len(xy)):
        dx, dy = xy[k, 0] - bx, xy[k, 1] - by
        d = np.sqrt(dx * dx + dy * dy)
        if d > lag:
            bx += dx * (1 - lag / d)
            by += dy * (1 - lag / d)
        out[k, 0], out[k, 1] = bx, by
    return out


# --- Simulation ------------------------------------------------------------------
@njit(cache=True)
def _relax(h, mask, i0, i1, j0, j1, tans, tanr, cx, cy, ball_r2, R, res, sweeps):
    """Angle of repose: slopes steeper than tans slide down to tanr (mass-conserving).
    Cells under the ball only take up sand to the ball surface."""
    dmax = tans * res
    drest = tanr * res
    for _ in range(sweeps):
        moved = 0.0
        for i in range(i0, i1):
            for j in range(j0, j1):
                if not mask[i, j]:
                    continue
                for di, dj in ((0, 1), (1, 0)):
                    a, b = i + di, j + dj
                    if a >= i1 or b >= j1 or not mask[a, b]:
                        continue
                    d = h[i, j] - h[a, b]
                    if d > dmax:
                        hi, hj, li, lj = i, j, a, b
                    elif d < -dmax:
                        hi, hj, li, lj = a, b, i, j
                        d = -d
                    else:
                        continue
                    q = 0.5 * (d - drest)
                    # limit space under the ball
                    rx = lj * res - cx
                    ry = li * res - cy
                    r2 = rx * rx + ry * ry
                    if r2 < ball_r2:
                        cap = R - np.sqrt(R * R - r2) - h[li, lj]
                        if cap <= 0:
                            continue
                        q = min(q, cap)
                    h[hi, hj] -= q
                    h[li, lj] += q
                    moved += q
        if moved < 1e-4:
            break


@njit(cache=True)
def _plow(h, mask, xs, ys, res, R, push_w, side, tans, tanr):
    n = h.shape[0]
    rr = int((R + push_w) / res) + 2
    rw = rr + int(3.0 / res)
    wbuf = np.zeros((2 * rr + 1, 2 * rr + 1))
    for k in range(1, len(xs)):
        cx, cy = xs[k], ys[k]
        dx, dy = cx - xs[k - 1], cy - ys[k - 1]
        L = np.hypot(dx, dy)
        if L < 1e-9:
            continue
        dx /= L
        dy /= L
        ci, cj = int(cy / res + 0.5), int(cx / res + 0.5)
        # 1) remove sand inside the ball volume
        removed = 0.0
        for i in range(max(ci - rr, 0), min(ci + rr + 1, n)):
            for j in range(max(cj - rr, 0), min(cj + rr + 1, n)):
                rx = j * res - cx
                ry = i * res - cy
                r2 = rx * rx + ry * ry
                if r2 < R * R:
                    s = R - np.sqrt(R * R - r2)
                    if h[i, j] > s:
                        removed += h[i, j] - s
                        h[i, j] = s
        # 2) deposit it as a bulge in front of and beside the ball
        if removed > 0:
            wsum = 0.0
            for i in range(max(ci - rr, 0), min(ci + rr + 1, n)):
                for j in range(max(cj - rr, 0), min(cj + rr + 1, n)):
                    w = 0.0
                    if mask[i, j]:
                        rx = j * res - cx
                        ry = i * res - cy
                        r = np.sqrt(rx * rx + ry * ry)
                        if R <= r < R + push_w:
                            c = (rx * dx + ry * dy) / r
                            w = max(0.0, c + side) * (1.0 - (r - R) / push_w)
                    wbuf[i - ci + rr, j - cj + rr] = w
                    wsum += w
            if wsum > 0:
                f = removed / wsum
                for i in range(max(ci - rr, 0), min(ci + rr + 1, n)):
                    for j in range(max(cj - rr, 0), min(cj + rr + 1, n)):
                        h[i, j] += f * wbuf[i - ci + rr, j - cj + rr]
        # 3) let it slide
        _relax(h, mask, max(ci - rw, 0), min(ci + rw + 1, n), max(cj - rw, 0),
               min(cj + rw + 1, n), tans, tanr, cx, cy, R * R, R, res, 3)


def simulate(tr, res=0.5, p=P, clear="auto", mirror=False, seed=1):
    n = int(2 * SAND_MM / res) + 1
    yy, xx = (np.mgrid[0:n, 0:n] * res - SAND_MM)
    mask = np.hypot(xx, yy) <= SAND_MM
    rng = np.random.default_rng(seed)
    h = p["h0"] + 0.05 * ndimage.gaussian_filter(rng.standard_normal((n, n)), 2) * 10
    h = np.where(mask, h, 0.0)
    if clear == "auto":
        clear = "out" if tr[0, 1] < 0.5 else "in"
    if clear in ("in", "out"):
        cp = clear_path(clear == "out", p["clear_pitch"])
        cp[:, 0] += tr[0, 0] - cp[-1, 0]     # continue seamlessly into the pattern start
        tr = np.vstack([cp, tr])
    xy = to_xy(resample(tr, p["step"]), mirror)
    if p["lag"] > 0:
        xy = _towed(xy, p["lag"])
    xy = xy + SAND_MM
    t = time.time()
    _plow(h, mask, xy[:, 0].copy(), xy[:, 1].copy(), res, BALL_R, p["push_w"],
          p["side"], np.tan(np.radians(p["stat"])), np.tan(np.radians(p["repose"])))
    # round edges (grains roll off), mass-conserving
    if p["smooth"] > 0:
        h = np.where(mask, ndimage.gaussian_filter(h, p["smooth"] / res), 0.0)
    print(f"simulation: {len(xy)} steps ({len(xy) * p['step'] / 1000:.0f} m), "
          f"{time.time() - t:.1f} s")
    return h, mask, xy[-1] - SAND_MM


# --- Rendering -------------------------------------------------------------------
L = dict(
    n_led=48,        # LED positions along the rim (strip = line source)
    r_led=318.0,     # radius of the LED strip [mm]
    z_led=25.0,      # height of the LEDs above the floor [mm]
    alpha0=30.0,     # vertical beam angle [°]
    falloff=1.2,     # falloff ~ 1/d^falloff
    shadow_mm=40.0,  # maximum shadow length [mm]
    soft=2.0,        # penumbra [°]
    ambient=0.3,     # ambient light (relative to direct light)
    ao=3.0,          # exponent of ambient occlusion in grooves
    ao_mm=15.0,      # range of ambient occlusion [mm]
    grain=0.01,      # grain (height noise) [mm]
    albedo_noise=0.2,
    # tone curve: relative brightness (median = 1) -> colour, obtained by quantile matching
    # (0.5 .. 99.5 %) against photos (top view orange, oblique view pink)
    tone_x=(0.176, 0.234, 0.307, 0.388, 0.553, 0.853, 1.399, 2.349, 3.022, 3.606, 4.136, 9.0),
    tone_top=((61, 25, 16), (73, 32, 22), (87, 39, 28), (101, 46, 32), (121, 55, 38),
              (146, 69, 46), (169, 82, 55), (194, 97, 65), (215, 109, 73),
              (233, 121, 84), (250, 136, 97), (255, 160, 125)),
    tone_oblique=((42, 27, 27), (56, 37, 37), (75, 50, 48), (93, 60, 56), (119, 73, 64),
                  (139, 87, 76), (160, 100, 87), (185, 116, 101), (209, 131, 112),
                  (233, 147, 126), (249, 163, 140), (255, 190, 170)),
)
BG = np.array([0.035, 0.025, 0.025])     # frame/surroundings


@njit(parallel=True, cache=True)
def _leds(H, nx, ny, nz, res, R0, leds, zl, a0, fall, smax, soft):
    """Direct light from the LED strip with cast shadows (horizon search)."""
    n = H.shape[0]
    out = np.zeros((n, n))
    nst = int(smax / res)
    for i in prange(n):
        y = R0 - i * res
        for j in range(n):
            x = j * res - R0
            if x * x + y * y > R0 * R0:
                continue
            h = H[i, j]
            acc = 0.0
            for k in range(leds.shape[0]):
                lx, ly = leds[k, 0] - x, leds[k, 1] - y
                d = np.sqrt(lx * lx + ly * ly)
                dz = zl - h
                lx /= d
                ly /= d
                el = np.arctan2(dz, d)
                em = np.exp(-(el / a0) ** 2) / d ** fall
                c = (nx[i, j] * lx + ny[i, j] * ly + nz[i, j] * dz / d) / np.sqrt(1 + (dz / d) ** 2)
                if c <= 0:
                    continue
                tmax = -1.0
                s = 1.0
                while s < nst:
                    ii = int(i - ly * s + 0.5)
                    jj = int(j + lx * s + 0.5)
                    if ii < 0 or jj < 0 or ii >= n or jj >= n:
                        break
                    t = (H[ii, jj] - h) / (s * res)
                    if t > tmax:
                        tmax = t
                    s += 1.0 + s * 0.08
                vis = min(1.0, max(0.0, (el - np.arctan(tmax)) / soft + 0.5))
                acc += c * em * vis
            out[i, j] = acc
    return out


@njit(parallel=True, cache=True)
def _ao(H, res, ndir, smax):
    """Sky visibility (cosine-weighted) from horizon angles in ndir directions."""
    n = H.shape[0]
    out = np.zeros((n, n))
    nst = int(smax / res)
    for i in prange(n):
        for j in range(n):
            h = H[i, j]
            v = 0.0
            for k in range(ndir):
                a = 2 * np.pi * (k + 0.5) / ndir
                dx, dy = np.cos(a), np.sin(a)
                tmax = 0.0
                s = 1.0
                while s < nst:
                    ii = int(i + dy * s + 0.5)
                    jj = int(j + dx * s + 0.5)
                    if ii < 0 or jj < 0 or ii >= n or jj >= n:
                        break
                    t = (H[ii, jj] - h) / (s * res)
                    if t > tmax:
                        tmax = t
                    s += 1.0 + s * 0.1
                v += 1.0 / (1.0 + tmax * tmax)   # cos²(horizon angle)
            out[i, j] = v / ndir
    return out


def shade(h, res, out_res, l=L, view="top", seed=2):
    """Lights the height field (row 0 = top/+y). Returns colour image, height field, mask."""
    n = int(2 * SAND_MM / out_res) + 1
    c = np.arange(n) * out_res / res
    H = ndimage.map_coordinates(h, np.meshgrid(c, c, indexing="ij"), order=3, mode="nearest")
    rng = np.random.default_rng(seed)
    gs = 0.125 / out_res                          # grain size ~0.25 mm
    # grain only in the normals, not in the shadows
    Hg = H + l["grain"] * 2 * ndimage.gaussian_filter(rng.standard_normal((n, n)), gs)
    gy, gx = np.gradient(Hg, out_res)
    nz = 1 / np.sqrt(1 + gx ** 2 + gy ** 2)
    nx, ny = -gx * nz, gy * nz                    # rows run towards -y
    phi = np.linspace(0, 2 * np.pi, l["n_led"], endpoint=False)
    leds = np.column_stack([np.cos(phi), np.sin(phi)]) * l["r_led"]
    R0 = (n - 1) * out_res / 2
    I = _leds(H, nx, ny, nz, out_res, R0, leds, l["z_led"], np.radians(l["alpha0"]),
              l["falloff"], l["shadow_mm"], np.radians(l["soft"]))
    yy, xx = np.mgrid[0:n, 0:n] * out_res - R0
    m = np.hypot(xx, yy) <= SAND_MM
    I /= np.percentile(I[m], 95)
    # ambient light, occluded in grooves and hollows
    I = I + l["ambient"] * _ao(H, out_res, 12, l["ao_mm"]) ** l["ao"] * nz
    alb = 1 + l["albedo_noise"] * ndimage.gaussian_filter(rng.standard_normal((n, n)), gs)
    I = np.clip(I * alb, 0, None)
    I /= np.percentile(I[m], 50)
    if l.get("raw"):
        return I, H, m
    rgb = np.array(l["tone_" + view]) / 255
    img = np.stack([np.interp(I, l["tone_x"], rgb[:, k]) for k in range(3)], -1)
    # darker edge towards the wall
    img *= np.clip((SAND_MM - np.hypot(xx, yy)) / 4, 0.35, 1)[..., None]
    img[~m] = BG
    return img, H, m


def ball_color(nx, ny, nz, sand):
    """Polished steel ball: reflects dark surroundings above, sand/LED light below."""
    env = sand * (0.25 + 0.75 * np.clip(1 - nz, 0, 1) ** 2) + 0.03
    hl = np.clip(0.3 * nx - 0.2 * ny + 0.93 * nz, 0, 1) ** 60
    return env + hl


def draw_ball_top(img, ball, out_res):
    n = img.shape[0]
    R0 = (n - 1) * out_res / 2
    i0, j0 = (R0 - ball[1]) / out_res, (ball[0] + R0) / out_res
    r = BALL_R / out_res
    ii, jj = np.mgrid[int(i0 - r) - 1:int(i0 + r) + 2, int(j0 - r) - 1:int(j0 + r) + 2]
    dx, dy = (jj - j0) / r, -(ii - i0) / r
    q = dx ** 2 + dy ** 2
    ok = (q < 1) & (ii >= 0) & (jj >= 0) & (ii < n) & (jj < n)
    sand = img[ii[ok], jj[ok]].mean(0) if ok.any() else np.array([0.6, 0.3, 0.2])
    nz = np.sqrt(np.clip(1 - q, 0, 1))
    col = ball_color(dx[ok][:, None], dy[ok][:, None], nz[ok][:, None], sand)
    img[ii[ok], jj[ok]] = np.clip(col, 0, 1)
    return img


# oblique-view camera, calibrated from a photo (Pixel phone, f ≈ 2940 px at 4080 px)
CAM = dict(pos=(-375.7, 58.2, 250.5), target=(-114.1, 31.0), hfov=69.5, size=(2040, 1536))


@njit(parallel=True, cache=True)
def _raycast(tex, H, res, R0, C, Rm, f, W, Hh, bg, ball, sand):
    n = H.shape[0]
    out = np.zeros((Hh, W, 3))
    zmax = max(H.max(), ball[2] + ball[3]) + 0.01
    zmin = H.min() - 0.01
    for v in prange(Hh):
        for u in range(W):
            # ray in world coordinates
            dc0, dc1 = (u - W / 2) / f, (v - Hh / 2) / f
            dx = Rm[0, 0] * dc0 + Rm[1, 0] * dc1 + Rm[2, 0]
            dy = Rm[0, 1] * dc0 + Rm[1, 1] * dc1 + Rm[2, 1]
            dz = Rm[0, 2] * dc0 + Rm[1, 2] * dc1 + Rm[2, 2]
            L = np.sqrt(dx * dx + dy * dy + dz * dz)
            dx, dy, dz = dx / L, dy / L, dz / L
            out[v, u, :] = bg
            if dz >= 0:
                continue
            # ball (ray-sphere intersection)
            ox, oy, oz = C[0] - ball[0], C[1] - ball[1], C[2] - ball[2]
            b = ox * dx + oy * dy + oz * dz
            disc = b * b - (ox * ox + oy * oy + oz * oz - ball[3] ** 2)
            tb = 1e30
            if disc > 0:
                tb = -b - np.sqrt(disc)
            t = (zmax - C[2]) / dz
            x, y, z = C[0] + t * dx, C[1] + t * dy, zmax
            st = 0.5 * res / max(np.sqrt(dx * dx + dy * dy), 1e-3)
            hit = False
            for _ in range(4000):
                if z < zmin or t > tb:
                    break
                fi, fj = (R0 - y) / res, (x + R0) / res
                i, j = int(fi), int(fj)
                if 0 <= i < n - 1 and 0 <= j < n - 1:
                    a, c = fi - i, fj - j
                    hz = (H[i, j] * (1 - a) * (1 - c) + H[i + 1, j] * a * (1 - c)
                          + H[i, j + 1] * (1 - a) * c + H[i + 1, j + 1] * a * c)
                    if z <= hz and x * x + y * y <= R0 * R0:
                        hit = True
                        break
                x += st * dx
                y += st * dy
                z += st * dz
                t += st
            if t > tb and tb < 1e29:
                # steel ball: reflection of sand (below) and dark surroundings
                px, py, pz = ox + tb * dx, oy + tb * dy, oz + tb * dz
                nzz = pz / ball[3]
                k = dx * px + dy * py + dz * pz
                rz = dz - 2 * k / ball[3] ** 2 * pz           # reflection direction z
                w = min(1.0, max(0.0, -rz * 4 + 0.6))
                for ch in range(3):
                    out[v, u, ch] = sand[ch] * (0.15 + 0.85 * w) + 0.03 + 0.6 * max(0.0, nzz) ** 40
            elif hit:
                i, j = int((R0 - y) / res + 0.5), int((x + R0) / res + 0.5)
                out[v, u, :] = tex[i, j, :]
    return out


def oblique(tex, H, res, ball, cam=CAM):
    """Perspective oblique view: trace rays against the height field."""
    W, Hh = cam["size"]
    f = W / 2 / np.tan(np.radians(cam["hfov"] / 2))
    tg = np.array([*cam["target"], 0.0])
    C = np.array(cam["pos"], float)
    fw = (tg - C) / np.linalg.norm(tg - C)
    rt = np.cross(fw, [0, 0, 1.0])
    rt /= np.linalg.norm(rt)
    Rm = np.array([rt, np.cross(fw, rt), fw])   # camera x, y (down), viewing direction
    R0 = (H.shape[0] - 1) * res / 2
    hm = H.mean()
    bl = np.array([ball[0], ball[1], BALL_R - hm, BALL_R])
    sand = np.percentile(tex[tex.sum(-1) > 0.3], 70, axis=0)
    return _raycast(tex, H - hm, res, R0, C, Rm, f, W, Hh, BG, bl, sand)


def rotate_cam(cam, deg):
    """Rotate the camera around the table centre."""
    a = np.radians(deg)
    R = np.array([[np.cos(a), -np.sin(a)], [np.sin(a), np.cos(a)]])
    p = np.array(cam["pos"])
    return dict(cam, pos=(*(R @ p[:2]), p[2]), target=tuple(R @ np.array(cam["target"])))


def render(thr, view="top", res=0.5, out_res=0.25, clear="auto", mirror=False,
           cam_rot=90.0, p=P, l=L):
    t = time.time()
    h, mask, ball = simulate(read_thr(thr), res, p, clear, mirror)
    img, H, m = shade(np.flipud(h), res, out_res, l, view)
    if view == "oblique":
        img = oblique(img, H, out_res, ball, rotate_cam(CAM, cam_rot))
    else:
        img = draw_ball_top(img, ball, out_res)
    print(f"total: {time.time() - t:.1f} s")
    return img


def save(img, path):
    Image.fromarray((np.clip(img, 0, 1) * 255 + 0.5).astype(np.uint8)).save(path)
    print("saved:", path)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("thr")
    ap.add_argument("-o", "--out", help="output image (default: <name>_sand.png or <name>_sand_oblique.png)")
    ap.add_argument("--view", default="top", choices=["top", "oblique"])
    ap.add_argument("--res", type=float, default=0.5, help="simulation grid [mm/px]")
    ap.add_argument("--out-res", type=float, default=0.25, help="image resolution [mm/px]")
    ap.add_argument("--clear", default="auto", choices=["auto", "in", "out", "none"],
                    help="clearing spiral before the pattern (auto: from outside if the pattern starts inside)")
    ap.add_argument("--mirror", action="store_true", help="mirror the theta direction")
    ap.add_argument("--cam-rot", type=float, default=90.0,
                    help="oblique view: rotate camera around the centre [°] (90: from below "
                         "as in the top view, 0: as in the calibration photo)")
    a = ap.parse_args()
    img = render(a.thr, a.view, a.res, a.out_res, a.clear, a.mirror, a.cam_rot)
    suffix = "_sand.png" if a.view == "top" else "_sand_oblique.png"
    save(img, a.out or str(Path(a.thr).with_suffix("")) + suffix)


if __name__ == "__main__":
    main()
