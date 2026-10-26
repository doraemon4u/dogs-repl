# OGS REPL/TUI 客户端 —— 开发步骤规划

> 本文件是 `OGS_REPL.md` 设计手册对应的执行计划。架构与技术决策以 `OGS_REPL.md` 为准,本文件只负责排期与验收标准。核心原则:任何依赖真实 OGS 服务器行为的假设,都要尽早用真实请求验证,不要留到最后才对接。

---

## 阶段总览

| 阶段 | 目标 | 关键产出 | 前置依赖 | 风险等级 |
|---|---|---|---|---|
| 0 | 验证真实 OGS API 连通性 | smoke-test 二进制 | 无 | 高(决定项目可行性) |
| 1 | Engine 层落地(离线) | Board / Coord / 规则 / SGF / 估分算法 | 无(可与阶段 0 并行) | 低 |
| 2 | Transport 层完整化 | 完整 RestClient / RealtimeClient + 协议同步脚本 | 阶段 0 | 高 |
| 3 | Session 层 | store / event bus / ChatScope | 阶段 1+2 | 中 |
| 4 | Presentation 层 | REPL 闭环 → TUI 渲染 → Estimating 模式 | 阶段 3 | 中 |
| 5 | 打磨与扩展 | 设计手册第 11 节待定事项逐项消化 | 阶段 4 | 低 |
| 6 | 发布 | 跨平台二进制 + 文档 | 阶段 5 | 低 |

---

## 阶段 0:真实 API 连通性验证(最优先,建议第一周完成)

**目标**:验证 `OGS_REPL.md` 第 2.2 节的认证假设(REST access token 与 Realtime JWT 是两套不同凭证)在真实服务器上成立。这一步产出一次性 smoke-test 程序,不是正式代码结构。

- [x] 在 `https://online-go.com/oauth2/applications/` 注册 OAuth2 应用,拿到 client_id(已建 public 应用,GRANT 为 Resource owner password based)
- [x] 换出 REST access token 并验证通路(实际验证:`grant_type=password` 直接换 token 成功,`/api/v1/me` 等端点 200;PKCE/授权码流程已放弃,`client_credentials` 返回 401 invalid_client;token 端点必须带 User-Agent,否则 Cloudflare 403)
- [x] 用同一凭证连接真实 socket 服务器,发送心跳/`hostinfo` 指令,确认收到匹配 id 的响应(token 可用于 REST + socket.io)

**为什么排最前**:若认证流程或协议版本与文档描述不符,`OGS_REPL.md` 第 3 节的分层设计、第 6 节的目录结构都可能要跟着调整,越早验证越省成本。

---

## 阶段 1:Engine 层(对应 OGS_REPL.md 第 7 节)—— 离线可测,零协议风险

可与阶段 0 并行推进。

- [x] `Board` 数据结构 + 坐标系统(跳 `I` 映射,7.1 节)
- [x] `Coord` 容忍解析器 + 单元测试(7.2 节代码可直接作为起点)
- [x] 提子 / 打劫 / 自杀规则校验
- [x] SGF 读写
- [x] 估分泛洪填色算法(10.1 节,暂不做死活判断)——实现为 flood-fill,29 个单元测试全部通过(board/coord/rules/sgf/estimate)

**验证方式**:从 OGS 网站导出真实对局 SGF 棋谱回放验证规则正确性——数据源头仍锚定真实平台,不是凭空造的测试用例。

---

## 阶段 2:Transport 层完整化(对应 OGS_REPL.md 第 2、5 节)

- [x] `RestClient` 补齐所有端点(挑战、锦标赛、死活题、用户资料等)——GET 路径已逐个对真实服务器探测确认(200/401/403 = 存在,404 = 不存在):`me` / `me/games` / `me/challenges` / `ui/overview` / `ui/config` / `challenges/` / `games/{id}/` / `games/{id}/sgf` / `tournaments/` / `tournaments/{id}/` / `puzzles/` / `puzzles/{id}/` / `players/`。注意 `players/{username}/` 不解析(404),详情路由只认数字 id,按名字查必须走 `players/?username=` 过滤
- [x] `RealtimeClient` 补齐 `protocol.ts` 全部 command/event,心跳、断线重连、请求 id 匹配机制——**机制已落地,协议已按 `goban` 源码核实**:
  - 请求/响应关联:`[command, data, id]` 下发、`[id, data, error]` 回包,`AtomicU64` 发号 + pending 表消费(单元测试覆盖)
  - 心跳 `net/ping` → `net/pong`;断线重连交给 `rust_socketio`,重连后补发 `authenticate` 并发送 `RealtimeEvent::Connected`
  - 传输层坑:OGS 对 polling 握手返回 `{"code":0,"message":"Transport unknown"}`,必须 `.transport_type(TransportType::Websocket)`,默认的 polling-first 升级路径永远连不上
  - 认证用 `GET /ui/config` 的 `user_jwt`,不是 REST access token
  - **线上协议以 `online-go/goban` 的 `GobanSocket` 为准**(OGS 官方参考实现),具体约定:
  - 下发:`cb ? [command, data, ++id] : data ? [command, data] : [command]`——带 id 是 request,不带是 notify
  - 收包:`const [id_or_command, data, err] = payload`;`typeof id_or_command === "number"` 就用 id 取回 callback(应答),否则当作推送事件 `emit(command, data)`
  - Rust 端按此实现:**直接判断数组首元素是不是数字**,不依赖 `rust_socketio` 把首元素为数字时回退成 `Event::Message` 的内部行为
  - `authenticate` 官方是 notify(拿不到回包);本项目改发 request 以便确认凭证被接受,`user/update` 推送也仍视为确认
  - `net/ping` 下发 `{client, drift, latency}`,`client` 必须是毫秒时间戳(服务端用它算 clock drift);`net/pong` 回 `{client, server}`
  - 传输层:OGS 拒绝 engine.io polling(`{"code":0,"message":"Transport unknown"}`),只能用 WebSocket
  - `["ERROR", ...]` 事件必须单独注册 `.on("error", ..)`:`rust_socketio` 会大小写不敏感地把 `"ERROR"` 折叠成 `Event::Error`,而 `on_any` 只收 `Event::Message | Event::Custom`
  - **`["ERROR", ...]` 事件收不到**:`rust_socketio` 的 `Event::from` 会大小写不敏感地把 `"ERROR"` 折叠成 `Event::Error`,而 `on_any` 只接收 `Event::Message | Event::Custom`,所以必须额外注册 `.on("error", ..)`,否则鉴权失败会完全静默
  - 已加 12 秒未登录看门狗,避免"连上了、能 pong"被误读为"可以用了"
  - **凭据已确认真实有效**:`/ui/config` 返回的 `user_jwt` 解出来是 `anonymous:false` + 真实 user id,该端点确实认 OAuth bearer(此前"只会拿到游客 jwt"的推测是错的)
  - **engine.io 握手会偶发失败**:`rust_socketio` 的 `IncompleteResponseFromEngineIo` 把真正原因吞掉,只显示 "EngineIO Error";且它的自动重连只覆盖"连上之后掉线",**首次握手失败完全不管**。已改为最多 5 次退避重连,并把底层 `rust_engineio::Error` 解包出来打日志
  - OGS 建连后会立刻推 `active-bots` 全量目录(实测 15 秒内 1231 帧),鉴权回包就淹没在里面——探测脚本必须留够读取窗口,否则会误判为"无回包"
  - 命令面覆盖 68 个 Goban command 中的 51 个(游戏控制/死活确认/撤销/聊天 PM/review/通知/计数订阅/stats/keepalive 等),REPL 可直接驱动全部对局类命令
  - 对照 `protocol.ts` 复查时发现并修正了 5 处字段错误:`user/monitor` 要 `user_ids: number[]`(不是 username)、`net/ping` 要 `{client,drift,latency}` 且 `client` 必须是毫秒时间戳、`gamelist/query` 的 `list` 只接受 `live|corr|kidsgo` 且 `where` 必填、`game/chat` 要 `{type,move_number,body}`、`chat/send` 要带 `uuid`
  - 待办:`automatch/*`、`bot/*`、`itc`、`remote_storage/*`、`seek_graph/*` 为非对局人机终端类功能(面向 bot / 匹配服务 / 云同步 / 棋谱图浏览器),由协议脚本标记为已知排除项
- [x] 落地第 5 节方案 A 的协议同步脚本(`scripts/sync_protocol.py`,支持 `--check` 与 `--json`,基于 conda `universe` 运行)

**关键原则**:每新增一个 command/event,都在真实沙盒账号上手动跑一次确认响应格式,不完全信任协议文档——协议文档与服务端实际实现之间可能存在出入。

---

## 阶段 3:Session 层(对应 OGS_REPL.md 第 8 节)

- [x] 全局 store、event bus(`src/session/store.rs`, `src/session/actor.rs`, `src/session/event_bus.rs`)
- [x] `ChatScope` 切换机制(World / Game)
- [x] 官方结算协商与死子状态同步(`apply_removed_stones`, 兼容数组与逗号/SGF 字符串)

依赖阶段 1(Engine 实例)与阶段 2(Realtime 事件)均完成。

---

## 阶段 4:Presentation 层(对应 OGS_REPL.md 第 9、10 节)—— 先闭环,后叠视觉

**4a. 纯文本 REPL 先行**
- [x] 不做 TUI 渲染时支持棋盘纯文本打印
- [x] 用 `[a10]` / `pass` / `resign` / `chat` 指令与 OGS 服务器交互

**4b. 叠加 `ratatui` 渲染**
- [x] 棋盘绘制(9.2 节,2 字符宽格子)
- [x] 9.5 节模块化 Pane 边框与焦点切换(Tab / Shift+Tab)
- [x] 9.3 节窗口尺寸检测与 resize 响应式布局

**4c. Estimating 模式**(第 10 节)
- [x] 复用 4b 棋盘渲染管线,MVP 手动标记死活(10.2 节),泛洪填色并修正顶底行坐标映射
- [x] 估分结果与官方死活结算完全解耦保持独立性

---

## 阶段 5:打磨与扩展(对应 OGS_REPL.md 第 11 节)

按优先级消化第 11 节清单:

- [x] 焦点面板切换按键绑定(Tab / Shift+Tab / Ctrl+1-3 / `focus <pane>`)
- [x] truecolor 检测与降级配色(`ColorSupport::detect()` 支持 TrueColor / Ansi256 / Ansi16)
- [x] 估分自动死活检测(迁移蒙特卡洛随机终局模拟逻辑 `estimate auto`,并保留 `[a10]` 手动切换)
- [x] 世界聊天语言频道动态拉取(支持 `/chat/channels/` 与 `/ui/config` 多形态解析及离线默认频道回退)
- [x] 时间制(byo-yomi/fischer/canadian)状态机与 SGF `TM`/`OT` 双向转换、对局时钟读秒同步
- [x] SGF 完整导入导出打磨(`sgf load <path>` / `sgf save <path>`、完整元信息读写、`step`/`prev`/`next`/`first`/`last`/`goto` 非破坏性复盘步进)

这些均不影响阶段 0–4 骨架的可用性,可按实际时间灵活排期。

---

## 阶段 6:发布

- [ ] 单二进制跨平台打包测试(Linux/macOS/Windows 终端兼容性)
- [ ] README / 使用文档

---

## 核心原则(贯穿全部阶段)

任何依赖真实 OGS 服务器行为的假设,都要尽早用真实请求验证,不要等到最后才对接——这是阶段 0 要抢在 Engine 层完整化之前做的根本原因,哪怕它产出的代码之后大部分会被推翻重写。
