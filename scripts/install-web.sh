#!/usr/bin/env bash
# DS Code Web — 一键安装脚本
# 构建前端 + 编译 Rust 服务 + 安装到 ~/.local，并生成启动命令。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
UI_DIR="$ROOT/crates/dscode-desktop/ui"
LIB_DIR="$HOME/.local/lib/dscode-web"
BIN="$HOME/.local/bin/dscode-web"

echo "==> DS Code Web 一键安装"
echo "    项目根目录: $ROOT"

# ── 1. 依赖检查 ──
if ! command -v cargo >/dev/null 2>&1; then
  echo "❌ 未找到 cargo，请先安装 Rust: https://rustup.rs"
  exit 1
fi
echo "  ✓ cargo: $(cargo --version)"

# ── 2. 构建前端（若 npm 可用；否则复用已有 dist）──
if command -v npm >/dev/null 2>&1; then
  echo "==> 构建前端 (Vite + React)"
  (cd "$UI_DIR" && npm run build)
else
  echo "⚠ 未找到 npm，跳过前端构建，使用已有 dist（若不存在则 Web 无界面）"
fi
if [ ! -d "$UI_DIR/dist" ]; then
  echo "❌ 前端 dist 不存在: $UI_DIR/dist"
  exit 1
fi

# ── 3. 编译 release 二进制 ──
echo "==> 编译 Rust 服务 (release)"
(cd "$ROOT" && cargo build --release -p dscode-web)

# ── 4. 安装 ──
echo "==> 安装到 $LIB_DIR"
mkdir -p "$LIB_DIR" "$HOME/.local/bin"
cp "$ROOT/target/release/dscode-web" "$LIB_DIR/dscode-web"
rm -rf "$LIB_DIR/dist"
cp -R "$UI_DIR/dist" "$LIB_DIR/dist"

# ── 5. 生成 wrapper（确保从任意目录启动都能定位 dist）──
cat > "$BIN" <<'WRAP'
#!/usr/bin/env bash
exec "$HOME/.local/lib/dscode-web/dscode-web" "$@"
WRAP
chmod +x "$BIN"

echo ""
echo "✅ 安装完成！"
echo ""
echo "  启动:  dscode-web"
echo "  地址:  http://127.0.0.1:8080"
echo ""
echo "  可选环境变量:"
echo "    DSCODE_WEB_ADDR=0.0.0.0:8080   局域网/外网访问（注意加 token）"
echo "    DSCODE_WEB_DIST=/path/to/dist  自定义前端目录"
echo ""
echo "  注意: 默认只绑定 127.0.0.1（仅本机访问）。若要暴露到网络，"
echo "        请配置反向代理认证，否则等于开放本机 shell。"
