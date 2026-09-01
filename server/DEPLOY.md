# 部署指南（公网 / 局域网）

服务端两个入口：

| 入口 | 端口 | 职责 |
|---|---|---|
| `llmserve.py` | 9001 (TCP) | 设备 WebSocket 对话 + 操作员 HTTP API + UDP Discovery |
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
## 3. 公网部署（Caddy 反代，推荐）

Caddy 自动签发 Let's Encrypt 证书，无需手动管证书。

**`/etc/caddy/Caddyfile`：**

```caddy
your.domain.example.com {
    # WebSocket 对话
    reverse_proxy /ws  127.0.0.1:9001

    # 操作员 API + 图片/OTA
    reverse_proxy /api/*  127.0.0.1:9001

    # 静态资源（如果有）
    # root * /var/www/youn
    # file_server
}
```

**防火墙：**

```bash
# 只放行 80（证书）、443（HTTPS/WSS）。9001/8766 不要暴露公网。
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
        proxy_pass http://127.0.0.1:9001;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_read_timeout 300s;
        proxy_send_timeout 300s;
    }

    location /api/ {
        proxy_pass http://127.0.0.1:9001;
        proxy_set_header X-Real-IP $remote_addr;
        client_max_body_size 32m;   # 图片/固件上传
    }
}
```

## 5. systemd 单元

**`/etc/systemd/system/youn-server.service`：**

```ini
[Unit]
Description=Youn Ink Server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=pi
WorkingDirectory=/home/pi/youn-ink-fourcolor-firmware/server
EnvironmentFile=/home/pi/youn-ink-fourcolor-firmware/server/.env
ExecStart=/home/pi/youn-ink-fourcolor-firmware/server/.venv/bin/python llmserve.py
Restart=on-failure
RestartSec=5
StandardOutput=append:/var/log/youn-server.log
StandardError=append:/var/log/youn-server.log

# 加固
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=/home/pi/youn-ink-fourcolor-firmware/server/data
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```


```bash
sudo systemctl daemon-reload
sudo systemctl enable --now youn-server
sudo systemctl status youn-server
```

## 6. 设备配对（首次）

1. 设备上电 → 上键+下键长按 1s 进 Wi-Fi 配网 → 连上你的 SSID
2. 设备 UDP 广播 `discover_host` → 服务器回 `discover_reply`
3. 设备 WS 连到 `PUBLIC_WS_URL` → 服务端创建 device（trust=0，未信任）
4. **在服务器上放行**：

```bash
curl -X POST http://127.0.0.1:9001/api/devices/<deviceId>/approve \
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
./start.sh logs              # 实时 tail
journalctl -u youn-server -f # systemd 模式
curl http://127.0.0.1:9001/api/health
curl http://127.0.0.1:9001/api/devices -H "X-Operator-Token: ..."
```

## 10. 安全清单（公网必须过一遍）

- [ ] `OPERATOR_TOKEN` 已设置（否则 /api/* 无鉴权，图片/OTA 上传对全网开放）
- [ ] `DISCOVERY_SHARED_SECRET` 已换成 32+ 字节随机值
- [ ] `.env` 权限 `chmod 600`，不入库
- [ ] 9001 / 8766 **不要直接暴露公网**——只放 80/443，反代到本地端口
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
curl http://127.0.0.1:9002/api/pages/schedule | python3 -m json.tool
```

返回包含 `schedule_md5`、`policy.sleep_window`、`policy.poll_interval_minutes`、
`policy.sleep_poll_interval_minutes`、`screen_active` 等字段。

### 删除页

```bash
curl -X DELETE http://127.0.0.1:9002/api/pages/morning \
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
curl http://127.0.0.1:9001/api/health
# {"status":"ok", ...}

# 本地设备发现：
python3 mock_client.py --server ws://127.0.0.1:9001/ws
```
