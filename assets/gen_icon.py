"""termpaint アイコン (512x512) の SVG を生成する。

コンセプト: ターミナルウィンドウの中に、一筆書きの「P」(Paint)。
縦棒の根元はセル単位の粗いブロックで、上に行くほど細かいピクセルに分かれ
(1セル → 2x2 → 4x4)、そこから先はピクセル単位の滑らかなブラシストロークになる。
書き終わりにはブラシカーソルのリング。

使い方 (リポジトリのルートで実行):
    python3 assets/gen_icon.py > assets/icon.svg
    inkscape assets/icon.svg --export-type=png --export-filename=assets/icon.png -w 512 -h 512
"""
import math, sys

S = 512
M = 16                 # 外周マージン
BODY = S - 2 * M       # 480
R = 104                # 角丸
TITLE_H = 64           # タイトルバーの高さ
CELL = 40              # 背景グリッドのセルサイズ
GX0, GY0 = M, M + TITLE_H  # グリッド原点
WIDTH = 40             # ストロークの太さ (= 1セル)

# 縦棒の根元のブロック: (y の上端, y の下端, ブロックサイズ, 隙間, 角丸)。境目はグリッドに揃える
BANDS = [(400, 440, 40, 4, 7), (360, 400, 20, 3, 4), (320, 360, 10, 2, 2)]
PIXEL_Y = BANDS[-1][0]   # これより上は滑らかなストローク

# 「P」の中心線。縦棒の中心はセルの中心 (x = 196) に置く
STEM_X = 196
K = 0.5523 * 24          # 角の丸み (半径 24) のベジェ係数
PATH = [
    ("L", (STEM_X, 420), (STEM_X, 164)),                                   # 縦棒
    ("C", (STEM_X, 164), (STEM_X, 164 - K), (STEM_X + 24 - K, 140), (STEM_X + 24, 140)),  # 左上の角
    ("L", (STEM_X + 24, 140), (300, 140)),                                 # 上辺
    ("C", (300, 140), (400, 140), (400, 296), (300, 296)),                 # ふくらみ
    ("L", (300, 296), (254, 296)),                                         # 下辺 (縦棒の手前で止める)
]

# 線の進み具合に沿った色
STOPS = [(0.0, "#22d3ee"), (0.30, "#6d8bff"), (0.66, "#e45fd0"), (1.0, "#ff9a3c")]


def lerp(a, b, t):
    return a + (b - a) * t


def sample(seg, n):
    kind, *p = seg
    for i in range(n + 1):
        t = i / n
        if kind == "L":
            yield (lerp(p[0][0], p[1][0], t), lerp(p[0][1], p[1][1], t))
        else:
            u = 1 - t
            yield tuple(u**3 * a + 3 * u * u * t * b + 3 * u * t * t * c + t**3 * d
                        for a, b, c, d in zip(*p))


pts = []
for k, seg in enumerate(PATH):
    s = list(sample(seg, 300))
    pts += s if k == 0 else s[1:]

# 弧長 (0..1)
acc = [0.0]
for (x0, y0), (x1, y1) in zip(pts, pts[1:]):
    acc.append(acc[-1] + math.hypot(x1 - x0, y1 - y0))
arc = [a / acc[-1] for a in acc]


def color(s):
    for (s0, c0), (s1, c1) in zip(STOPS, STOPS[1:]):
        if s <= s1:
            t = (s - s0) / (s1 - s0)
            rgb = [round(lerp(int(c0[i:i+2], 16), int(c1[i:i+2], 16), t)) for i in (1, 3, 5)]
            return "#%02x%02x%02x" % tuple(rgb)
    return STOPS[-1][1]


def nearest_arc(x, y):
    i = min(range(len(pts)), key=lambda i: (pts[i][0] - x) ** 2 + (pts[i][1] - y) ** 2)
    return arc[i]


def rasterize():
    out = []
    r = WIDTH / 2
    for y_top, y_bot, cell, gap, rr in BANDS:
        cells = set()
        for x, y in pts:
            if y < y_top - r or y > y_bot + r:
                continue
            for cx in range(int((x - r - GX0) // cell), int((x + r - GX0) // cell) + 1):
                for cy in range(int((y - r - GY0) // cell), int((y + r - GY0) // cell) + 1):
                    ccx = GX0 + cx * cell + cell / 2
                    ccy = GY0 + cy * cell + cell / 2
                    if y_top <= ccy < y_bot and math.hypot(ccx - x, ccy - y) <= r:
                        cells.add((cx, cy))
        for cx, cy in sorted(cells):
            bx = GX0 + cx * cell + gap / 2
            by = GY0 + cy * cell + gap / 2
            c = color(nearest_arc(bx + cell / 2, by + cell / 2))
            out.append(f'<rect x="{bx:.2f}" y="{by:.2f}" width="{cell-gap}" height="{cell-gap}" rx="{rr}" fill="{c}"/>')
    return out


blocks = rasterize()

# 滑らかな部分: PIXEL_Y より上。短い線分に分けて、線に沿って色を変える
i0 = next(i for i, p in enumerate(pts) if p[1] <= PIXEL_Y)
smooth = list(range(i0, len(pts)))
STEP = 4
segs = []
for a in range(0, len(smooth) - 1, STEP):
    ia, ib = smooth[a], smooth[min(a + STEP, len(smooth) - 1)]
    (xa, ya), (xb, yb) = pts[ia], pts[ib]
    segs.append(f'<line x1="{xa:.2f}" y1="{ya:.2f}" x2="{xb:.2f}" y2="{yb:.2f}" stroke="{color(arc[ia])}"/>')
d = "M " + " L ".join(f"{pts[i][0]:.2f} {pts[i][1]:.2f}" for i in smooth[::3] + [smooth[-1]])
end = pts[-1]
ring_r = WIDTH / 2 + 13

grid = []
for i in range(1, BODY // CELL):
    x = GX0 + i * CELL
    grid.append(f'<line x1="{x}" y1="{GY0}" x2="{x}" y2="{S-M}"/>')
for j in range(1, (S - M - GY0) // CELL + 1):
    y = GY0 + j * CELL
    grid.append(f'<line x1="{M}" y1="{y}" x2="{S-M}" y2="{y}"/>')

svg = f'''<svg xmlns="http://www.w3.org/2000/svg" width="{S}" height="{S}" viewBox="0 0 {S} {S}">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#232838"/>
      <stop offset="1" stop-color="#10131b"/>
    </linearGradient>
    <linearGradient id="bar" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#313749"/>
      <stop offset="1" stop-color="#272c3c"/>
    </linearGradient>
    <radialGradient id="gridfade" cx="0.4" cy="0.75" r="0.85">
      <stop offset="0" stop-color="#fff" stop-opacity="1"/>
      <stop offset="1" stop-color="#fff" stop-opacity="0.12"/>
    </radialGradient>
    <mask id="gridmask"><rect x="0" y="0" width="{S}" height="{S}" fill="url(#gridfade)"/></mask>
    <mask id="strokemask"><path d="{d}" fill="none" stroke="#fff" stroke-width="{WIDTH}" stroke-linecap="round" stroke-linejoin="round"/></mask>
    <clipPath id="above"><rect x="0" y="0" width="{S}" height="{PIXEL_Y}"/></clipPath>
    <clipPath id="body"><rect x="{M}" y="{M}" width="{BODY}" height="{BODY}" rx="{R}"/></clipPath>
    <filter id="glow" x="-30%" y="-30%" width="160%" height="160%">
      <feGaussianBlur stdDeviation="16"/>
    </filter>
    <filter id="shadow" x="-10%" y="-10%" width="120%" height="120%">
      <feGaussianBlur stdDeviation="3"/>
    </filter>
  </defs>

  <g clip-path="url(#body)">
    <rect x="{M}" y="{M}" width="{BODY}" height="{BODY}" fill="url(#bg)"/>

    <!-- 背景のセルグリッド -->
    <g stroke="#ffffff" stroke-opacity="0.06" stroke-width="1.5" mask="url(#gridmask)">
      {"".join(grid)}
    </g>

    <!-- タイトルバー -->
    <rect x="{M}" y="{M}" width="{BODY}" height="{TITLE_H}" fill="url(#bar)"/>
    <rect x="{M}" y="{M+TITLE_H-1.5}" width="{BODY}" height="1.5" fill="#000" fill-opacity="0.35"/>
    <circle cx="{M+62}" cy="{M+TITLE_H/2}" r="10" fill="#ff5f57"/>
    <circle cx="{M+94}" cy="{M+TITLE_H/2}" r="10" fill="#febc2e"/>
    <circle cx="{M+126}" cy="{M+TITLE_H/2}" r="10" fill="#28c840"/>

    <!-- プロンプト "> _" -->
    <polyline points="{GX0+30},{GY0+30} {GX0+52},{GY0+48} {GX0+30},{GY0+66}" fill="none"
              stroke="#d6dcea" stroke-width="9" stroke-linecap="round" stroke-linejoin="round"/>
    <rect x="{GX0+68}" y="{GY0+60}" width="34" height="9" rx="3" fill="#d6dcea"/>

    <!-- グロー -->
    <g opacity="0.5" filter="url(#glow)">
      <g clip-path="url(#above)" fill="none" stroke-width="{WIDTH}" stroke-linecap="round">{"".join(segs)}</g>
    </g>
    <g opacity="0.3" filter="url(#glow)">
      {"".join(blocks)}
    </g>

    <!-- 縦棒の根元: セル → ピクセル -->
    {"".join(blocks)}

    <!-- 滑らかなストローク -->
    <g clip-path="url(#above)">
      <g fill="none" stroke-width="{WIDTH}" stroke-linecap="round">
        {"".join(segs)}
      </g>
      <g mask="url(#strokemask)">
        <path d="{d}" fill="none" stroke="#ffffff" stroke-opacity="0.25" stroke-width="10" stroke-linecap="round"
              stroke-linejoin="round" transform="translate(-5,-9)"/>
      </g>
    </g>

    <!-- ブラシカーソルのリング -->
    <circle cx="{end[0]:.1f}" cy="{end[1]:.1f}" r="{ring_r}" fill="none" stroke="#000" stroke-opacity="0.45" stroke-width="7" filter="url(#shadow)"/>
    <circle cx="{end[0]:.1f}" cy="{end[1]:.1f}" r="{ring_r}" fill="none" stroke="#ffffff" stroke-width="5"/>
  </g>

  <!-- 外周のハイライト -->
  <rect x="{M+0.75}" y="{M+0.75}" width="{BODY-1.5}" height="{BODY-1.5}" rx="{R-0.75}" fill="none"
        stroke="#ffffff" stroke-opacity="0.10" stroke-width="1.5"/>
</svg>
'''
sys.stdout.write(svg)
