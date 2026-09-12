# 每台设备一套页面（per-device pages）设计

日期：2026-09-12
状态：待复核
上游：本 spec 实现用户在 `/images` 上提出的「替换页面应该先选择设备」

## 问题

设备现在共享**一套全局页面**：

- 页面源平铺在 `data/pages/{name}.json`，`PageSource` 没有归属字段（`pages.py:39-46`）。
- `GET /api/pages/schedule`（`app.py:619-641`）不带任何设备参数，所有设备拉到同一组页。
- 已注册设备 1 台（`NOTE4C-3400FC`），所以今天"选设备"选不选都一样。

用户要的是每台设备各有一套页面：先选设备，再选该设备的页面（或在其中选一页替换画面）。

**关键前提（已核实）**：设备拉 schedule 时已经带着自己的 Bearer token（`firmware/main/rust/src/page_sync.rs:171` 调 `rf_http_get(url, token, ...)`），服务端也有现成的 `_require_device_token(request) -> Device`（`app.py:107-121`）⇒ **按设备分组不需要改固件**。

## 目标

1. 页面严格归属于一台设备（一对多，不允许共享）。
2. 设备只轮播自己的那组页；轮换进度**按设备独立**计算。
3. Web 端先选设备，再在其页面上操作（管理页与上传页一致）。
4. **固件零改动**；迁移后 `NOTE4C-3400FC` 的 `schedule_md5` 与现在**逐字节相同**。

## 非目标

跨设备共享同一页；"立刻只在某台设备上显示"；设备管理（配对/信任）界面；S3/CDN；任何固件改动。

## 一、数据模型与存储

```
data/pages/
  NOTE4C-3400FC/
    logo-1024.json          ← 页面源：归属 = 所在目录（唯一真相）
    page2.json
  36be550c….bin             ← 位图：全局内容寻址，跨设备共享一份
  36be550c….bmp.json        ← 位图 meta：sources 用复合键
```

- **归属由目录表达，不写进 JSON** ⇒ 页面 JSON 的 schema 不变，迁移只需移动文件（避免两份真相不一致）。
- **`sources` 复合键**：`"<device_id>/<name>"`（如 `"NOTE4C-3400FC/logo-1024"`）。引用计数逻辑本身不改（`_record_source_reference` / `_drop_source_reference` 已按字符串比较，`pages.py:219-253`）。
- 位图保持全局 ⇒ 同内容跨设备不重复占空间。

### 代码触点（逐函数）

| 位置 | 现在 | 改为 |
|---|---|---|
| `PageSource`（`pages.py:39-46`） | `name/canvas_json/duration_minutes/order` | 增加 `device: str`；**不入 JSON**，由加载路径注入 |
| `_page_path(name)`（`pages.py:83-88`） | `pages/{name}.json` | `pages/{device}/{name}.json`，device 与 name 都过同一套字符白名单 |
| `_all_page_sources()`（`pages.py:114-124`） | `glob("*.json")` | 遍历 `pages/*/` 子目录（非递归两层），从目录名注入 `device` |
| `_all_bitmap_metas()`（`pages.py:128-137`） | `glob("*.json")` | **不变**（非递归 ⇒ 设备子目录天然不可见，meta 仍平铺） |
| `build_schedule_from_disk(device)`（`pages.py:284-301`） | 无参，匹配 `src.name in sources` | 加 `device` 参数；只取该设备的源；匹配复合键 |
| `_is_page_source` / `_is_bitmap_meta` | — | 不变（后缀判定仍成立） |
| `get_bitmap`（`pages.py:202-210`） | 限制在 `pages/` 内 | 不变（位图仍在那里） |

## 二、接口

| 端点 | 变化 |
|---|---|
| `GET /api/pages/schedule` | **要求设备 Bearer token**（设备本来就带），用它解出设备后返回该设备那组；`current_index` / `seconds_until_next_page` 在该设备的列表上计算 |
| `GET /api/pages?device=<id>` | 新增查询参数（operator 认证不变） |
| `POST /api/pages` | body 增加必填 `device`；缺失 ⇒ 400 |
| `DELETE /api/pages/{name}?device=<id>` | 须指明归属；页面不属于该设备 ⇒ 404 |
| `POST /api/uploads` | multipart 增加必填 `device`；`page` **必须属于该设备**，否则 400 并返回该设备的可选页面 |
| `GET /api/pages/bitmap/{md5}.bin` | **不变**（内容寻址、免认证保持现状；设备只会从自己的 schedule 拿到 md5） |
| `POST /api/pages/preview` | 不变（只吃 canvas） |

设备身份只从 token 取（不引入 `device_id` 查询参数），管理员接口的设备则来自显式参数——两者互不混淆。

**管理员接口的设备校验（三个端点一致）**：`device` 必须存在于设备注册表且 `trusted`（`registry.get(device)`，`app.py` 现有 helper 语义），否则 400；`GET /api/pages` 与 `DELETE` 缺 `device` 同样 400。理由：不做这条校验的话，拼错设备名会凭空造出一个新目录 = 一套幽灵页面集。

## 三、Web 端

- 顶部一个**全局设备选择器**：设备来自 `GET /api/devices`，**只列已信任的**；选择存 `localStorage`。
- 「页组管理」与「替换页面画面」都跟随它；**未选设备时两页都显示「请先选择设备」并禁用操作**（与上传的"必须绑定"同一条纪律）。
- 「替换页面画面」的链路因此是：**选设备 → 选该设备页面 → 选图 → 上传替换**。
- `api.js` 的 `pages()` / `createPage` / `deletePage` / `uploadToPage` 都带上 `device`。

## 四、迁移（一次性、可审阅、可回滚）

1. `git mv server/data/pages/logo-1024.json server/data/pages/NOTE4C-3400FC/logo-1024.json`，`page2.json` 同理。
2. 三个被跟踪的 meta 的 `sources` 改成复合键：
   - `36be550c….bmp.json`: `["logo-1024"]` → `["NOTE4C-3400FC/logo-1024"]`
   - `9670217f….bmp.json`: `["logo-1024"]` → **清空该项**（见下）
   - `9c9c526b….bmp.json`: `["page2"]` → `["NOTE4C-3400FC/page2"]`
3. **顺手修掉一处已存在的不一致**：`36be550c` 与 `9670217f` 两张 meta 都声称拥有 `logo-1024`（历史残留，正是刚修复的"一页一位图"不变量要清的）。schedule 当前取 `max((rendered_at, md5))` ⇒ 实际生效的是 `36be550c`。迁移时把 `9670217f` 的 `sources` 清空并删除该位图（内容是当前 logo 的确定性渲染，可随时重渲 ⇒ 无信息损失）。

回滚 = `git revert` 迁移提交（数据在 git 里）。

## 五、验收（逐条可测）

1. **设备侧零扰动**：迁移后 `GET /api/pages/schedule`（带设备 token）的 `schedule_md5` == `de3dc3118cd0d350e94dee7cfc54a7be`（迁移前实测值），且 `pages` 列表与现在一致。用 curl 实测，不靠推算。
2. 设备照常轮换：设备日志继续出现 `schedule updated: N pages` 与 `show page i/N`。
3. 按设备隔离：给设备 B 建页后，A 的 schedule 不含 B 的页，反之亦然（pytest）。
4. `POST /api/uploads` 指定属于另一台设备的页面 ⇒ 400，且不写任何文件。
5. 未带设备 token 请求 schedule ⇒ 401（设备自身不受影响）。
6. Web：未选设备时两页禁用；选中设备后下拉里只有该设备的页面（浏览器实测）。

## 六、测试

服务端 pytest（沿用 `conftest.py` 的 `data_dir`/`uploads_dir` 隔离）：

1. 两台设备各自的页面互不出现（`build_schedule_from_disk` 层面 + API 层面各一条）。
2. schedule 缺 token ⇒ 401；token 属于未信任设备 ⇒ 401。
3. `POST /api/pages` 缺 `device` ⇒ 400。
4. 上传到别台设备的页面 ⇒ 400 且 `uploads_dir` 无新增（先校验后落盘）。
5. `sources` 复合键的引用计数：删除一个页面只让对应 bitmap 的 refcount 减一，别的设备同内容页面不受影响。
6. 迁移不变量：单独一条测试针对**真实跟踪数据**（把仓库的 `data/pages` 拷进临时目录）断言 `NOTE4C-3400FC` 的 entries 与迁移前一致 —— 这条防止将来有人手滑改坏存储布局。

Web：`npm run build` + 浏览器实测（选择器、禁用态、下拉内容）。
真机：curl 核 `de3dc311…` 不变 + 短窗串口确认设备照常轮换。

## 七、风险

- **迁移动到被跟踪数据**：`git mv` 与 meta 重写都在 git 里，可 revert；验收第 1 条是硬闸。
- **兼容性**：新代码读旧布局（缺设备子目录）时行为如何？规定：**迁移必须与代码同批落地**，且新代码对"页面在平铺层"的情况直接忽略（不做兼容读），避免两套布局同时存在。
- **`schedule_position` 的周期语义**：按设备独立后，两台设备的 `current_index` 各自算，这本身无风险；但设备数增多时 schedule 端点的扫描成本随设备子目录数线性增加（当前 1 台，可忽略）。
- **免认证的位图端点**保留：任何知道 md5 的人都能取该位图（既有设计取舍，本次不改）。
