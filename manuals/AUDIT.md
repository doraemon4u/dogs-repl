# OGS REPL/TUI 客户端 —— 架构与开发流程审计报告

> 审计对象:`OGS_REPL.md`(设计手册)、`OGS_repl_process.md`(开发步骤规划)
> 审计目的:检查两份文档之间的一致性、开发顺序上的依赖逻辑,以及与真实 OGS 平台对接的假设是否站得住脚
> 状态标记说明:`[ ]` = 尚未处理,分析/解决方案留待后续补充

---

## 一、严重发现(功能性缺口,会导致项目跑不通)

### 1. 对局的"官方结算"流程没有被设计覆盖
- [x] **已解决**:
  - `src/session/store.rs` 实现了 `apply_removed_stones` 与 `scoring_dead` 状态集,并完整支持数组、逗号分隔及紧凑 SGF 字符串解析。
  - `src/transport/realtime_client.rs` 提供了官方终局协议命令:`game/removed_stones/set`、`game/removed_stones/accept`、`game/removed_stones/reject`。
  - `src/ui/commands.rs` 提供了 REPL 命令 `score accept`、`score reject`、`score dead <coords>`。
  - `src/engine/estimate.rs` 严格区分了本地临时形势估分(`estimate`)与双人官方结算协商(`score`)。

### 2. Realtime JWT 的获取方式被当成已解决问题,实际存疑
- [x] **阶段 0 验证结果**:REST API 连通正常(200 OK);OAuth2 password grant type 可用,直接获取 access_token + refresh_token;token 是随机字符串而非标准 JWT,但可直接用于 REST API 认证和 socket.io 连接
- 应用设置要求:Client Type = public,Authorization Grant Type = Resource owner password based,Redirect URIs 留空
- token endpoint: `https://online-go.com/oauth2/token/`(末尾 `/` 必须带)
- 请求必须带 User-Agent header,否则被 Cloudflare WAF 拦截(403 error 1010)

### 3. OAuth2 令牌刷新流程完全缺失
- [x] **阶段 0 验证结果**:refresh_token 随 access_token 一起返回;auth.rs 已实现 refresh() 函数,用 grant_type=refresh_token 换新 token
- token 有效期 2592000 秒(30 天),auth.rs 在过期前 300 秒自动刷新

---

## 二、中等发现(架构留白,实现阶段容易变成返工)

### 4. Session 层的并发所有权模型未指定
- [x] **已解决**:在 `src/session/actor.rs` 采用单持有者 Actor 模式(`SessionActor`),外部通过 `SessionHandle` 的 mpsc channel 派发异步命令;状态查询通过 `Arc<RwLock<Store>>` 保证 TUI 渲染无锁竞争与单向依赖。

### 5. REST API 限流/退避策略未提及
- [x] **已解决**:在 `src/transport/rest_client.rs` 中实现了针对 HTTP 429 与 5xx 的自适应退避与重试机制(`retry_with_backoff`),并覆盖了单元测试。

### 6. `rust_socketio` 与 OGS 实际协议版本的兼容性只是假设,缺验证步骤
- [x] **2026-10 更正**:OGS Realtime 不是 Socket.IO,而是 `wss://online-go.com` 上的纯 WebSocket,每条消息是一个 JSON 数组(与 `goban` 的 `GobanSocket` 一致)。Socket.IO 帧下服务器仍会推送聊天,但从不应答请求,导致 `authenticate` 永远收不到回包、登录后无法在世界频道发言。已改用 `tokio-tungstenite` 直连,`authenticate` 在每次(重)连后最先发送,认证成功后才加入频道。
- [x] **2026-10 频道聊天确认**:实测 OGS 会把 `chat/send` 转发给频道里的其他人,但**从不回显给发送方的 socket**,所以原先"等待自己的回显、10 秒没等到就报 `not confirmed`"每次都误报(消息其实已发出,游客连接能在频道历史里看到)。现在 `chat/send` 带请求 id 发送,以服务器的 `[rid, null, null]` 回包作为确认,`[rid, null, error]` 时显示拒绝原因,10 秒无回包才警告。消息 id 改为与网页客户端 `chatSoftUid` 相同的格式(62 进制用户 id + `.` + 62 进制毫秒时间戳):旧的 `ogs-tui-N` 每次启动从头计数,会与频道历史里已有的 id 重复,而客户端会丢弃 id 已见过的消息。以下旧记录仅供参考。
- [x] **已解决**:在 `src/transport/realtime_client.rs` 中明确指定了 `.transport_type(TransportType::Websocket)`,修正了 `[command, data, id]` 与 `[id, data, err]` 首元素数字/字符串分发,注册了单独的 `.on("error")` 拦截,并增加了自动重连后补发 `authenticate` 与 `RealtimeEvent::Connected` 事件机制。

### 7. 凭证持久化的安全存储方式未设计
- [x] **已解决**:在 `src/config.rs` 中设计了多层级凭据配置系统,持久化到用户主目录下并在 Unix 系统上限制文件权限为 0600,优先读取环境变量。

---

## 三、轻微发现(文档一致性/措辞问题)

### 8. 两份文档之间有一条过时的冲突 TODO
- [x] **已解决**:对齐了 `OGS_REPL.md` 与 `OGS_repl_process.md`,过时条目已标注由阶段规划取代。

### 9. 棋盘固定宽度的"动态性"未显式写清楚
- [x] **已解决**:在 `src/ui/tui/layout.rs` 中将 `board_width` 实现为根据当前活跃对局尺寸(`size`)动态计算(`3 + u16::from(size) * 2`),并在每帧渲染时响应式计算。

### 10. 阶段 4a"完整下完一整局"的里程碑缺测试对手来源
- [x] **已明确**:明确采用 OGS 平台的官方 AI 对手(如 KataBot)或测试沙盒账号。

### 11. 协议同步脚本没有明确的运行时机
- [x] **已解决**:落地了第 5 节方案 A 的独立 Python 协议同步脚本 `scripts/sync_protocol.py`,支持 `--check` 与 `--json`,使用 conda `universe` 环境执行,已在 CI/哨兵巡检就绪。

---

## 四、验证通过、未发现问题的部分

- [x] 分层依赖方向在两份文档中完全一致(Engine 无 IO → Session 依赖 Engine+Transport → Presentation 依赖 Session),`process.md` 各阶段前置依赖与此严丝合缝,无循环依赖或依赖倒置
- [x] 坐标解析"语法层(Presentation)vs 语义层(Engine)"的分工全文档无自相矛盾之处
- [x] Estimating 功能本身(排除第一条服务端协商缺口)的"读数据 → 算归属 → 切换渲染源"三段式设计自洽,复用已有棋盘渲染管线,未引入新架构层
- [x] resize 边界情况(零尺寸、拖拽防抖)覆盖了关键场景,无遗漏

---

*本文件仅记录审计发现,不包含解决方案——具体修复方式待后续分析后再补充到本文件或对应设计/流程文档中。*
