# OGS REPL/TUI 客户端设计手册

> 目标:构建一个逻辑架构与 [online-go/online-go.com](https://github.com/online-go/online-go.com)(OGS 官方 Web 客户端)一致的命令行 TUI-REPL 围棋客户端,尽可能覆盖 CLI 场景下可实现的全部功能。

---

## 1. 项目目标

- 复刻 OGS Web 客户端的**逻辑分层**,而不是照搬其 UI 组件结构
- 在纯终端环境(无鼠标)下,提供体验尽量接近网页客户端的对局、观战、聊天、大厅功能
- 通过分层架构保证协议层可独立维护、引擎层可独立测试

---

## 2. OGS 平台架构背景

### 2.1 REST API 与 Realtime API 双通道

OGS 的客户端与服务端通信分两条通道:

| 通道 | 用途 | 特点 |
|---|---|---|
| **REST API**(apiary 文档) | 低频操作:登录、创建/接受挑战、锦标赛、梯队、死活题、用户资料等 | 请求-响应模式 |
| **Realtime API**(基于 Socket.IO) | 高频/实时操作:落子、看棋、聊天、通知推送、对局心跳保活 | 长连接,JSON 消息 |

Realtime 消息格式:
- 客户端 → 服务端:`{ command, data, id? }`,`id` 可选,若携带则服务端会回一条带相同 `id` 的响应
- 服务端 → 客户端:`{ event_name, data }`,或对某个带 `id` 请求的响应

**关键约束**:即使是"实时对局",官方客户端也依赖持续的 websocket 连接做 keep-alive——断线会被服务端判定为客户端离线,进行中的挑战会被清除。因此本项目的 Realtime 客户端必须内置**心跳与自动重连**机制。

### 2.2 认证机制

- REST API:OAuth2(支持 PKCE)
- Realtime API:需要从 OAuth 流程中换取一个 **JWT**,在建立 socket 连接时作为认证信息传入——与 REST 使用的普通 access token 不是同一套凭证

### 2.3 参考实现:goban

官方维护的 TypeScript 库 [`online-go/goban`](https://github.com/online-go/goban) 是棋盘引擎 + Realtime 协议客户端的官方参考实现,不含 UI。协议一旦升级,`goban` 会最先同步更新,是本项目协议兼容性的权威参照。

---

## 3. 总体分层架构

```
┌─────────────────────────────────────────┐
│  Presentation Layer (TUI + REPL)         │
│  - REPL 命令解析器                        │
│  - TUI 渲染(棋盘/落点列表/聊天/通知)       │
├─────────────────────────────────────────┤
│  Session / Store Layer                   │
│  - 当前用户、好友、通知、棋局列表状态       │
│  - 事件总线(分发 socket 事件给订阅者)      │
├─────────────────────────────────────────┤
│  Engine Layer(对应官方 goban)             │
│  - 棋盘状态机、提子/打劫规则、终局计分      │
│  - SGF 读写、时间制(byo-yomi/fischer 等)   │
├─────────────────────────────────────────┤
│  Transport Layer                         │
│  - RestClient(OAuth2+PKCE,低频操作)       │
│  - RealtimeClient(socket.io,JSON 协议)    │
└─────────────────────────────────────────┘
```

依赖方向单向向下:UI 依赖 Session,Session 依赖 Engine/Transport,**Engine 不依赖任何 IO**——可脱离网络单独用 SGF 棋谱做规则测试。

---

## 4. 技术栈

选型依据:该场景是 I/O 密集型(等网络、等键盘输入)而非计算密集型,选 Rust 主要是为了并发模型、单一静态二进制、内存安全,而非纯粹算力。

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
reqwest = { version = "0.12", features = ["json"] }     # REST + OAuth2 PKCE
rust_socketio = { version = "0.6", features = ["async"] } # Realtime API
serde = { version = "1", features = ["derive"] }
serde_json = "1"
ratatui = "0.29"       # TUI 渲染
crossterm = "0.28"     # 终端后端 + resize 事件
reedline = "0.38"       # REPL 输入行(历史/补全)
```

`rust_socketio` 支持 Socket.IO 协议 v5(对应 Engine.IO v4),满足 OGS 服务端要求的最低协议版本。

---

## 5. 协议兼容性维护策略

手写 Rust 协议层的风险是:OGS Realtime 协议没有官方跨语言 SDK,唯一保真实现是 TypeScript 的 `goban`。两种应对方案:

**方案 A(默认推荐):纯 Rust + 协议同步脚本**
写一个独立的 Python/Node 小脚本,定期拉取 `online-go/goban` 仓库的 `protocol.ts`,解析出 command/event 名称并与 Rust 端 `serde` 结构体做 diff,协议变化时报警提醒更新。脚本不参与运行时,只是"兼容性哨兵"。保留单二进制、无额外运行时依赖的优势。

**方案 B(备选):Rust 前端 + Node.js sidecar**
Rust 只做 REPL/TUI,真正的协议通信交给常驻的 Node 子进程,内部直接 `import` 官方 `goban` 处理协议转发,与 Rust 端通过本地 Unix domain socket 传 JSON-RPC。兼容性等于与官方 100% 同步,代价是多一个运行时依赖和进程间通信开销(本机场景通常可忽略)。

> 建议先按方案 A 起步做 MVP,若后续协议维护成本过高,再切换到方案 B——这个切换点只影响 Transport 层,其余三层代码不受影响。

---

## 6. 目录结构建议

```
ogs-tui/
├── transport/
│   ├── auth.rs           # OAuth2 PKCE + JWT
│   ├── rest_client.rs
│   └── realtime_client.rs # socket.io 封装,含心跳/重连
├── engine/
│   ├── board.rs
│   ├── coord.rs           # 坐标解析与校验
│   ├── rules.rs           # 中国/日本规则、打劫、点目
│   ├── sgf.rs
│   └── clock.rs           # byo-yomi/fischer/canadian
├── session/
│   ├── store.rs
│   └── event_bus.rs
├── ui/
│   ├── repl/               # 命令解析、命令表
│   └── tui/                # 棋盘渲染、分屏布局、resize 处理
└── main.rs
```

---

## 7. Engine 层设计

### 7.1 棋盘规格与坐标系统

围棋记谱惯例:列字母跳过 `I`(避免与数字 1 混淆),行号从棋盘底部数起,范围 1..board_size。

| 规格 | 列字母范围 | 字母数 |
|---|---|---|
| 9x9 | A–H, J | 9 |
| 13x13 | A–H, J–N | 13 |
| 19x19 | A–H, J–T | 19 |

字母到列索引的映射(跳过 I):

```rust
fn col_index(letter: char) -> u8 {
    let letter = letter.to_ascii_uppercase();
    if letter > 'I' { letter as u8 - b'A' - 1 } else { letter as u8 - b'A' }
}
```

### 7.2 坐标输入语法与容忍解析算法

**语法**:落子指令为方括号包裹的"字母+数字",字母大小写不敏感,字母与数字之间允许任意数量空白(不要求恰好 1 个,100 个以上也应正常解析)。

```
[a10]   [A10]   [a 10]   [a          10]   [q13]
```

**解析实现**(手写状态机,不引入 `regex` crate——语法足够简单,手写更省依赖、报错更精确):

```rust
struct RawCoord { letter: char, number: u16 }

fn parse_coord(input: &str) -> Result<RawCoord, ParseError> {
    let trimmed = input.trim();
    let inner = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .ok_or(ParseError::MissingBrackets)?;

    let mut chars = inner.trim().chars().peekable();
    let letter = chars.next().ok_or(ParseError::Empty)?;
    if !letter.is_ascii_alphabetic() {
        return Err(ParseError::InvalidLetter);
    }

    let rest: String = chars.skip_while(|c| c.is_whitespace()).collect();
    if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
        return Err(ParseError::InvalidNumber);
    }

    Ok(RawCoord { letter: letter.to_ascii_uppercase(), number: rest.parse()? })
}
```

**校验分工**:
- **Presentation 层**只做语法解析(是否形如 `[字母 数字]`),不知道棋盘规格
- **Engine 层**做语义校验:结合当前对局的 `board.size`,判断列字母是否越界、是否跳过 `I`、该点是否已有子、是否违反打劫/自杀规则

**需要显式拒绝并给出明确报错的边界情况**:空方括号 `[ ]`、字母后接非法字符(如 `[ab]`)、数字后有多余字符(如 `[a10x]`)、坐标超出当前棋盘规格范围。

**非坐标类指令**:`pass`、`resign`、`undo` 等不使用方括号语法,作为独立 REPL 命令词,与坐标输入(方括号语法)并列存在,输入时无需额外前缀,直接按 pattern 路由。

---

## 8. Session 层设计

### 8.1 全局状态

维护跨屏共享的状态:参与中的棋局、旁观中的棋局、挑战列表、聊天频道、未读通知。收到 RealtimeClient 事件后更新对应 Engine 实例,再发布给订阅的 UI 组件(观察者模式,单用户 CLI 场景无需引入完整 Flux/Redux)。

### 8.2 聊天范围切换(ChatScope)

主页(大厅)与对局内看到的是同一个聊天面板,区别只是订阅范围:

```rust
enum ChatScope {
    World { channel: LanguageChannel }, // 主页:按语言分的世界频道
    Game { game_id: u64 },              // 对局内:仅对局双方 + 观战者发言
}
```

切屏时只切换 `ChatScope`,**不断开 world chat 订阅**——后台持续接收消息、未读计数正常累积,切回主页时聊天记录不丢失。

> 语言频道的具体列表(有哪些语言、频道 id)应在运行时通过 Realtime API 动态拉取,不在客户端硬编码——避免频道增减导致列表过期。这部分应纳入第 5 节"协议同步脚本"的覆盖范围。

---

## 9. Presentation 层设计(TUI + REPL)

### 9.1 UI 布局

```
┌─────────────────────┬───────────────────────┐
│                      │  落点节点列表 + 对局信息 │
│                      │                       │
│      棋盘(左)        ├───────────────────────┤
│                      │                       │
│                      │       聊天区域          │
└─────────────────────┴───────────────────────┘
```

- 左:棋盘
- 右上:整局落点(节点)列表 + 对局信息
- 右下:聊天区域(主页 = 世界频道聊天;对局内 = 仅对局双方 + 观战者)

### 9.2 终端字符宽高比处理

多数终端字体宽高比接近 1:2,若每个交叉点只用 1 字符宽度绘制,棋盘会显得又窄又高。解决方式:**每个交叉点横向使用 2 字符宽度**(线条组合或全角棋子字符 ● ○ 天然占 2 格宽),使视觉比例接近真实正方形棋盘。19x19 场景下,棋盘区域建议预留至少 45–50 列宽。

### 9.3 窗口尺寸检测与响应式布局

**启动检查**:用 `crossterm::terminal::size()` 获取当前终端尺寸,与"当前场景所需最小尺寸"比较。最小尺寸**不是全局常量**,取决于当前界面(大厅/对局)及对局中的棋盘规格,应封装为 `min_size_for(scene) -> (u16, u16)`,启动、resize 时都调用。低于最小值时展示"当前 {w}x{h},建议至少 {min_w}x{min_h},建议全屏"提示,并持续监听后台 resize 事件,满足条件后自动切回正常界面。

**运行时 resize 响应**:`crossterm` 将 resize 作为独立事件(`Event::Resize(w, h)`)与键盘事件走同一事件流。每一帧都用当前实际尺寸重新计算布局(`ratatui::Layout::split()` 天然支持):
- 棋盘面板:`Constraint::Length(board_width)`——固定字符宽度,不随窗口拉伸变形
- 右侧面板:`Constraint::Min(0)` 吃掉剩余空间,内部再按比例(如 6:4)分落点列表与聊天
- 若窗口小于棋盘所需宽度,直接触发"尺寸不足"提示,不强行挤压棋盘

**边界情况**:
- 最小化/异常尺寸:部分终端模拟器会在最小化时发出极小甚至 0 的 resize 事件;渲染循环需防御性判断 `area.width == 0 || area.height == 0` 时跳过本帧绘制,避免布局计算在零尺寸上 panic
- 拖拽调整窗口的事件风暴:连续 resize 事件应做防抖(30–50ms 内无新事件再重绘),可用 `tokio::select!` 配合定时器实现,与主事件循环(键盘/网络/resize 三路事件)统一处理

### 9.4 REPL 命令示例(草案)

```
ogs> login
ogs> lobby
ogs> challenge create --opponent kata --rules chinese --board 19
ogs> game 12345678
ogs[12345678]> [q16]
ogs[12345678]> pass
ogs[12345678]> resign
ogs[12345678]> chat gg
```

### 9.5 模块化 Pane 布局(Zellij 风格边框)

**边框实现**:`ratatui` 的 `Block` widget 原生支持独立边框颜色与嵌入标题栏,和 Zellij Pane Frames 效果一致,不需要自己实现绘制逻辑:

```rust
Block::default()
    .title(" 棋盘 ")
    .borders(Borders::ALL)
    .border_type(BorderType::Rounded)
    .border_style(Style::default().fg(Color::Cyan))
```

对棋盘 / 落点节点列表 / 聊天三个 `Rect` 各包一层 `Block`,即可实现三模块独立分区,且边框随各自 `Rect` 一起响应 9.3 节的 resize 逻辑,不需要额外处理。建议封装统一辅助函数,避免三处重复:

```rust
fn render_pane<W: Widget>(frame: &mut Frame, area: Rect, title: &str, color: Color, inner: W) {
    let block = Block::default()
        .title(format!(" {title} "))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(color));
    frame.render_widget(block.clone(), area);
    frame.render_widget(inner, block.inner(area));
}
```

**颜色方案**:
- 三个模块使用跨色相组合(如青/黄/品红,或蓝/橙/绿),避免红绿搭配对色盲用户不友好
- 启动时的环境自检(与 9.3 节的窗口尺寸检测合并为同一步骤)读取 `COLORTERM` 判断终端是否支持真彩色:支持则用 `Color::Rgb(r,g,b)` 精确配色,不支持则降级为 `ratatui` 内置命名色,保证低配终端下三个框依然可区分

**主页与对局页的容器策略差异**:
- 主页(大厅):所有信息共享**一个**边框,内部用简单分隔线或 `Tabs` 做次级区隔——大厅信息以浏览为主,不需要频繁切换焦点
- 对局/学习页:拆成**三个独立**边框模块(棋盘/落点节点/聊天),各自带标题和专属颜色——三块内容并行、需要分别关注

这个差异本身也是一种隐性交互信号:框越多代表模块越独立、可能需要单独操作;框少或合一代表内容以整体浏览为主。

**焦点面板标识**(为后续滚动交互预留):REPL 无鼠标,若聊天/落点列表未来支持上下滚动(翻聊天记录、翻棋谱),需要"当前焦点在哪个框"的概念,建议用 `Tab` 或 `Ctrl+1/2/3` 切换焦点,焦点框在色相不变的基础上叠加亮度/粗细变化(非焦点暗色细框,焦点亮色粗框)——颜色代表"是哪个模块",亮度/粗细代表"当前归它管"。

**REPL 输入行是否加框**:待定,倾向不加框、作为贯穿底部的无边框状态栏,保持"随时可打字"的轻量感;若追求与 Zellij 更一致的视觉统一性,也可以作为第四个窄条模块加框,两种方案架构上都不冲突。

---

## 10. 官子形势判断(Estimating)

### 10.1 归属判断算法

核心是**区域填色(Area Scoring / Tromp-Taylor 式泛洪填充)**,不是直接用"气"来算——"气"只是死活判断环节的辅助信号。纯几何判断分两步:

1. 对棋盘上每一片连通空点区域做 BFS/DFS 泛洪填充,收集边界相邻棋子的颜色集合
   - 边界只接触黑子 → 黑地
   - 边界只接触白子 → 白地
   - 两种颜色都接触 → 中立点(dame)
2. 复杂度 O(棋盘点数),9x9/13x13/19x19 三种规格复用同一套代码,仅数组大小不同

### 10.2 死活判断(真正的难点)

泛洪填充本身不知道"这几颗子其实已经死了"。两种路线:

- **完整方案**:蒙特卡洛随机对局模拟(从当前局面跑大量随机收官,统计每点最终归属概率),`goban` 库大概率采用类似思路——若要与官方观感一致,建议直接迁移 `goban` 里 estimate 相关模块的逻辑,而非自研算法
- **MVP 方案**:不做自动死活检测,完全交给用户手动标记——用户在估分模式下用已有的坐标语法 `[a10]` 点某个子,切换它所在整个连通块的死/活状态,每次切换重跑一遍 10.1 的泛洪填色。这条交互链路完全复用现有坐标解析器,只是解析结果从"落子"改为"切换死活标记"

自动死活检测可作为 MVP 之后的迭代项,不阻塞功能可用性。

### 10.3 数据结构与渲染

Engine 层新增独立模块(如 `estimate.rs`),输入 `Board` + 用户标记的死子集合,输出与棋盘等大的估算结果数组,不做任何渲染:

```rust
enum PointEstimate {
    BlackTerritory,
    WhiteTerritory,
    Dame,               // 中立点
    LiveStone(Color),
    DeadStone(Color),   // 仍记录原色,方便切换回活
}
```

渲染复用 9.2 节已有的棋盘绘制管线(2 字符宽格子),仅在估分模式下把数据源从"当前落子状态"切换成 `PointEstimate` 数组:

- 空点判黑地 → 暗色小方块
- 空点判白地 → 浅色小方块
- 中立点 → 保持空白
- 活子 → 正常画棋子
- 死子 → 棋子颜色调暗或叠加标记(如 `╳`)

**配色需与 9.5 节的 Pane 边框颜色系统分离**:棋盘内部的地盘标记建议用灰阶(深灰/浅灰),而不是边框那套高辨识度色相——边框颜色代表"是哪个模块",棋盘内容本身的黑白灰阶代表"棋盘信息",两套配色语义不应混用。

### 10.4 交互流程

```
game[12345678]> estimate          # 进入估分模式,棋盘切换渲染方式
game[12345678]> [c14]             # 切换该点所在连通块的死活标记,实时重算重画
game[12345678]> [c14]             # 再次点击切回活状态
game[12345678]> estimate confirm  # 确认标记,按区域填色规则计算比分
game[12345678]> estimate exit     # 不提交,退出估分模式,棋盘恢复正常显示
```

整个功能对已有架构零侵入:Engine 层新增一个纯函数模块,Presentation 层复用现有棋盘渲染管线切换数据源,REPL 层复用已有坐标解析器——唯一真正的新工程量是"是否实现自动死活检测"这一决策点。

---

## 11. 待定事项 / Roadmap

- [x] Realtime 协议字段的完整清单(照 `goban` 的 `protocol.ts` 逐条迁移)
- [x] 协议同步脚本的具体实现(拉取频率、diff 报警方式:`scripts/sync_protocol.py --check|--json`)
- [x] 方案 A vs 方案 B 的最终取舍时机(采用方案 A 半自动同步脚本)
- [x] SGF 导入/导出的完整支持范围(支持 `sgf load/save`、完整元信息、`TM`/`OT` 时间制与 `step`/`prev`/`next`/`first`/`last`/`goto` 复盘跳转)
- [x] 时间制(byo-yomi/fischer/canadian)在 Engine 层的状态机设计
- [x] 世界聊天语言频道列表的动态拉取与展示方式(支持 `/chat/channels/`、`/ui/config` 字典/数组及内置默认语言频道回退)
- [x] MVP 优先级排序:建议第一阶段先落地 `Coord` 解析器 + `Board` 校验(Engine 层最容易独立测试的部分)
- [x] Pane 焦点切换的按键绑定最终选定(Tab / Shift+Tab / Ctrl+1-3 / `focus <pane>`)
- [x] REPL 输入行是否加框的最终决定(保持底部 2 行无框状态栏 + 输入栏)
- [x] 终端 truecolor 检测与降级配色的具体实现(`ColorSupport::detect()`)
- [x] estimate 自动死活检测(蒙特卡洛模拟,迁移自 `goban`)是否纳入 MVP 后第一阶段(`estimate auto` 已实现并接入 TUI/REPL)

---

*本文件根据设计讨论整理,后续决策变化以本文件更新为准。*
