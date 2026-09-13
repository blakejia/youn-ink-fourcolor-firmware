# 部署指南（公网 / 局域网）

服务端两个入口：

| 入口 | 端口 | 职责 |
|---|---|---|
| `llmserve.py` | 9002 (TCP) | 设备 WebSocket 对话 + 操作员 HTTP API + UDP Discovery |
| `push_image.py` | 8766 (TCP+UDP) | 图片上传/推送 + OTA + UDP Discovery（**与 llmserve 二选一，两个进程不能同时跑**） |

> **注意**：两个入口都包含 UDP Discovery + 完整 HTTP API。跑一个就够。
> 推荐**只跑 `llmserve.py`**，它已经覆盖了全部功能。

## 1. 安装

```bash
cd server
python3 -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt

cp .env.example .env
# 编辑 .env：
#   OPENAI_API_KEY=sk-xxx
#   DISCOVERY_SHARED_SECRET=...  (32+ 字节随机，openssl rand -hex 32)
#   PUBLIC_WS_URL=wss://your.domain/ws
#   PUBLIC_HTTP_BASE=https://your.domain
#   OPERATOR_TOKEN=<openssl rand -hex 32>   ← 不设置则 operator API 无鉴权，公网绝对不能裸跑
```

## 2. 局域网裸跑（先跑通再说）

```bash
cd server
./start.sh install      # 只需一次：装单元、enable、开 linger
./start.sh start
curl http://127.0.0.1:9002/api/health
```

设备配网时服务端地址填 `http://<本机局域网 IP>:9002`。详见 §5。

## 3. 公网部署（Caddy 反代，推荐）

Caddy 自动签发 Let's Encrypt 证书，无需手动管证书。

**`/etc/caddy/Caddyfile`：**

```caddy
your.domain.example.com {
    # WebSocket 对话
    reverse_proxy /ws  127.0.0.1:9002

    # 操作员 API + 图片/OTA
    reverse_proxy /api/*  127.0.0.1:9002

    # 静态资源（如果有）
    # root * /var/www/youn
    # file_server
}
```

**防火墙：**

```bash
# 只放行 80（证书）、443（HTTPS/WSS）。9002/8766 不要暴露公网。
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
sudo ufw enable
```

**UDP Discovery 是内网协议**——公网设备走 `PUBLIC_WS_URL` 直连 WSS，不依赖广播。
只有同网段设备会从 UDP Discovery 拿到 `wsUrl`，所以 `.env` 里的
`PUBLIC_WS_URL` / `PUBLIC_HTTP_BASE` 必须填公网域名（或公网 IP）。

## 4. Nginx 替代方案

```nginx
server {
    listen 443 ssl http2;
    server_name your.domain.example.com;

    ssl_certificate     /etc/letsencrypt/live/your.domain/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/your.domain/privkey.pem;

    location /ws {
        proxy_pass http://127.0.0.1:9002;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_read_timeout 300s;
        proxy_send_timeout 300s;
    }

    location /api/ {
        proxy_pass http://127.0.0.1:9002;
        proxy_set_header X-Real-IP $remote_addr;
        client_max_body_size 32m;   # 图片/固件上传
    }
}
```

## 5. systemd 服务

单元文件**版本化在仓库里**，`start.sh` 负责安装（符号链接进 user manager，保持单一真相源）：

```
server/systemd/youn-ink-server.service               ← 改这里
~/.config/systemd/user/youn-ink-server.service       → 符号链接
```

```bash
cd server
./start.sh install      # 建链接 + enable + 开启 linger
./start.sh start
./start.sh status
./start.sh logs         # 应用日志（data/server.log，10 MB × 5 自动轮转）
./start.sh journal      # stdout/stderr（访问日志、traceback）
```

**这台机器没有 root**，所以服务跑在 **user manager** 里，并已开启
`loginctl enable-linger pi`：开机自启 ✓、退出登录后继续运行 ✓。

几个不能改错的地方：

- `WorkingDirectory` 必须是 `server/` —— 应用按工作目录解析 `.env` 和 `data/`。
- `Restart=always` + `RestartSec=5`：崩溃和意外退出都会拉回；`systemctl stop` 不会
  （systemd 对被显式停止的单元不再重启）。
- **日志分两路，不要合并**：应用自己用 `RotatingFileHandler` 写 `data/server.log`；
  stdout/stderr 交给 journal。若把 systemd 的 stdout 也 `append:` 到同一个文件，
  轮转后 systemd 会继续写被重命名过的旧 inode，轮转等于失效。
- `ProtectSystem=strict` 会把整个文件系统挂成只读，唯一可写区由 `ReadWritePaths`
  指定为 `server/data`（数据库、页面、上传、固件、日志都在其中）；`PrivateTmp` 提供
  可写 `/tmp`。
- 单元是 `Type=simple`：`systemctl start` 在进程起来时就返回，**端口就绪还要 2–4 s**
  （uvicorn 绑定 + MCP/session manager 初始化）。开机后立刻打开后台可能看到一次连接
  失败，刷新即可；设备侧本来就会重试。

<details>
<summary>有 root 的机器（可选：系统级单元）</summary>

复制同一份单元，把 `WantedBy=default.target` 改成 `multi-user.target`、补上
`User=<运行用户>`，放到 `/etc/systemd/system/`，再 `sudo systemctl enable --now
youn-ink-server`。user 单元里的 `StartLimit*` 同理保留在 `[Unit]` 段。
</details>

## 6. 设备配对（首次）

1. 设备上电 → 上键+下键长按 1s 进 Wi-Fi 配网 → 连上你的 SSID
2. 设备 UDP 广播 `discover_host` → 服务器回 `discover_reply`
3. 设备 WS 连到 `PUBLIC_WS_URL` → 服务端创建 device（trust=0，未信任）
4. **在服务器上放行**：

```bash
curl -X POST http://127.0.0.1:9002/api/devices/<deviceId>/approve \
     -H "X-Operator-Token: $OPERATOR_TOKEN"
```

5. 设备重连，成功进入对话页面

## 7. 图片上传

```bash
curl -X POST https://your.domain/api/images \
     -H "X-Operator-Token: $OPERATOR_TOKEN" \
     -F "image=@photo.jpg" \
     -F "format=bwry2bpp" \
     -F "title=周末随拍" \
     -F "target_device_id=<deviceId>"
```

服务端会：
1. Pillow 解码 → 400×300 fit → Floyd-Steinberg 抖动 → 打包 2bpp BWRY（30000 字节）
2. 存入 `data/images/<id>.bin` + `<id>.json`
3. 如果目标设备 WS 在线 → 主动推 `image_push_meta` + 分块二进制 + `image_push_done`

## 8. OTA

```bash
# 上传新固件（.bin 是合并镜像，写 0x0 那种）
curl -X POST https://your.domain/api/ota \
     -H "X-Operator-Token: $OPERATOR_TOKEN" \
     -F "firmware=@merged-binary.bin" \
     -F "version=6.5.9-note4c-51812e4" \
     -F "channel=stable" \
     -F "notes=first build"

# 设备查询
curl https://your.domain/api/ota/check
# {"available":true,"version":"...","sha256":"...","url":"https://your.domain/api/ota/download/xxx.bin"}
```

服务端自动算 SHA-256，用 `DISCOVERY_SHARED_SECRET` 做 HMAC 签名，下载时**重新验签**——
磁盘上固件被篡改会直接拒绝下发。

## 9. 日志 / 诊断

```bash
./start.sh logs              # 跟随应用日志（data/server.log，10 MB × 5 自动轮转）
./start.sh journal           # 跟随 stdout/stderr（uvicorn 访问日志、traceback）
./start.sh status            # 单元状态（退出码跟 systemd 一致）
journalctl --user -u youn-ink-server -f      # 同上，直接走 journalctl
systemctl --user show youn-ink-server -p NRestarts --value   # 崩溃重启次数
curl http://127.0.0.1:9002/api/health
curl http://127.0.0.1:9002/api/devices -H "X-Operator-Token: ..."
```

## 10. 安全清单（公网必须过一遍）

- [ ] `OPERATOR_TOKEN` 已设置（否则 /api/* 无鉴权，图片/OTA 上传对全网开放）
- [ ] `DISCOVERY_SHARED_SECRET` 已换成 32+ 字节随机值
- [ ] `.env` 权限 `chmod 600`，不入库
- [ ] 9002 / 8766 **不要直接暴露公网**——只放 80/443，反代到本地端口
- [ ] 设备 `trust=1` 后才允许推送/OTA；未知 deviceId 不 approve 就行
- [ ] `latest.json` + `.sig` 由服务端写入，下载时重新验签
- [ ] HTTPS/WSS 走 Caddy 或 nginx，Let's Encrypt 证书自动续期
- [ ] 日志轮转（`RotatingFileHandler` 已配，10 MB × 5）

## 11. Canvas Loop（画板页组）

设备侧新增 `page_sync` 模块后，服务端需要向设备暴露 `/api/pages/schedule` 和
`/api/pages/bitmap/{md5}.bin`。设备每 `poll_interval_minutes` 轮询一次 schedule，
休眠窗口内放宽到 `sleep_poll_interval_minutes` 且不刷屏。

### 上传页定义

```bash
curl -X POST http://127.0.0.1:9002/api/pages \
     -H "X-Operator-Token: $OPERATOR_TOKEN" \
     -H 'Content-Type: application/json' \
     -d '{
       "device": "NOTE4C-3400FC",
       "name": "morning",
       "canvas_json": {
         "default": [{
           "type": "div",
           "props": {
             "tw": "flex flex-col p-[20px] gap-[10px] bg-black",
             "style": {"color": "#FFFFFF"},
             "children": [
               {"type": "div", "props": {"tw": "text-[24px] font-bold", "children": "早上好"}},
               {"type": "div", "props": {"tw": "bg-yellow w-[200px] h-[80px]", "children": ""}},
               {"type": "div", "props": {"tw": "bg-red w-[200px] h-[80px]", "children": ""}}
             ]
           }
         }]
       },
       "duration_minutes": 10,
       "order": 0
     }'
```

### 查询当前 schedule
```bash
# schedule 是设备接口：需要该设备的 Bearer token（配对后签发），不是 operator header。
# DEVICE_TOKEN=<pair-start/claim 流程签发的设备 token>
curl http://127.0.0.1:9002/api/pages/schedule \
     -H "Authorization: Bearer $DEVICE_TOKEN" | python3 -m json.tool
```

返回包含 `schedule_md5`、`policy.sleep_window`、`policy.poll_interval_minutes`、
`policy.sleep_poll_interval_minutes`、`screen_active` 等字段。

### 删除页

```bash
curl -X DELETE "http://127.0.0.1:9002/api/pages/morning?device=NOTE4C-3400FC" \
     -H "X-Operator-Token: $OPERATOR_TOKEN"
```

### 支持的 Canvas 元素

`div` / `span` / `img`，固定尺寸 `w-[Npx]` / `h-[Npx]`，flex 布局
（`flex-row` / `flex-col` / `gap-[Npx]` / `p-[Npx]`），背景色
`bg-{white|black|red|yellow}`，边框 `border` / `rounded-[Npx]`，字体大小
`text-[Npx]`，图片 `data:` URI / `http(s)://` URL。

**不支持**：`$for` / `$ifAny` / `{{get}}` 模板，z-index / 3D transform /
calc()，CSS 动画/过渡。

### 位图地址

设备用 `GET /api/pages/bitmap/{md5}.bin` 直接拉取 30000 字节的 2bpp BWRY 位
图，内容寻址、无需鉴权。


```bash
./start.sh start
curl http://127.0.0.1:9002/api/health
# {"status":"ok", ...}

# 本地设备发现：
python3 mock_client.py --server ws://127.0.0.1:9002/ws
```
