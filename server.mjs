import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PORT = Number(process.env.PORT || 3000);

const SOURCE_FILES = [
  'src/engine/estimate.rs',
  'src/engine/clock.rs',
  'src/engine/game.rs',
  'src/engine/sgf.rs',
  'src/engine/board.rs',
  'src/engine/coord.rs',
  'src/engine/rules.rs',
  'src/session/event_bus.rs',
  'src/session/store.rs',
  'src/session/actor.rs',
  'src/ui/commands.rs',
  'src/ui/tui/app.rs',
  'src/ui/tui/widgets.rs',
  'src/ui/tui/board_render.rs',
  'src/ui/tui/estimate_render.rs',
  'src/ui/repl/mod.rs',
  'manuals/OGS_REPL.md',
  'manuals/OGS_repl_process.md',
  'manuals/AUDIT.md',
];

function safeResolve(relPath) {
  const cleaned = String(relPath || '').replace(/^\/+/, '');
  const resolved = path.resolve(__dirname, cleaned);
  if (!resolved.startsWith(__dirname)) {
    return null;
  }
  return resolved;
}

const HTML_PAGE = `<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>OGS REPL & TUI Client</title>
  <meta name="description" content="Terminal and REPL client for Online-Go.com (OGS) featuring a pure Go engine, Monte-Carlo dead-stone territory estimation, SGF import/export with review stepping, and Zellij-inspired TUI panes." />
  <meta property="og:title" content="OGS REPL & TUI Client" />
  <meta property="og:description" content="Terminal and REPL client for Online-Go.com (OGS) featuring a pure Go engine, Monte-Carlo dead-stone territory estimation, SGF import/export with review stepping, and Zellij-inspired TUI panes." />
  <style>
    :root {
      --bg: #0d1117;
      --panel-bg: #11161f;
      --text: #e6edf3;
      --muted: #7d8590;
      --board: #58a6ff;
      --moves: #d29922;
      --chat: #3fb950;
      --lobby: #a371f7;
      --error: #f85149;
      --warning: #e3b341;
      --border-dim: #30363d;
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      background: var(--bg);
      color: var(--text);
      font-family: "JetBrains Mono", "Fira Code", "SFMono-Regular", Consolas, monospace;
      font-size: 13px;
      height: 100vh;
      display: flex;
      flex-direction: column;
      overflow: hidden;
    }
    header {
      display: flex;
      align-items: center;
      justify-content: space-between;
      padding: 6px 12px;
      background: #161b22;
      border-bottom: 1px solid var(--border-dim);
      flex-shrink: 0;
    }
    .brand {
      display: flex;
      align-items: center;
      gap: 10px;
      font-weight: 700;
      color: var(--board);
    }
    .tabs {
      display: flex;
      gap: 6px;
    }
    .tab-btn {
      background: #21262d;
      color: var(--text);
      border: 1px solid var(--border-dim);
      padding: 4px 10px;
      cursor: pointer;
      font-family: inherit;
      font-size: 12px;
    }
    .tab-btn.active {
      border-color: var(--board);
      color: var(--board);
      font-weight: 700;
    }
    .quick-actions {
      display: flex;
      gap: 6px;
      flex-wrap: wrap;
    }
    .quick-btn {
      background: #1f242c;
      color: #c9d1d9;
      border: 1px solid #30363d;
      padding: 3px 8px;
      cursor: pointer;
      font-family: inherit;
      font-size: 11px;
    }
    .quick-btn:hover {
      border-color: var(--board);
      color: #fff;
    }
    #tui-view {
      flex: 1;
      display: grid;
      grid-template-columns: calc((100% - 12px) * 2 / 3) minmax(0, 1fr) minmax(0, 1fr);
      gap: 6px;
      padding: 6px;
      min-height: 0;
    }
    .pane {
      background: var(--panel-bg);
      border: 1px solid var(--border-dim);
      border-radius: 4px;
      display: flex;
      flex-direction: column;
      min-height: 0;
      overflow: hidden;
    }
    .pane.focused.board-pane { border: 2px solid var(--board); }
    .pane.focused.moves-pane { border: 2px solid var(--moves); }
    .pane.focused.chat-pane { border: 2px solid var(--chat); }
    .pane-header {
      padding: 4px 8px;
      font-weight: 700;
      border-bottom: 1px solid #21262d;
      font-size: 12px;
      display: flex;
      justify-content: space-between;
    }
    .board-pane .pane-header { color: var(--board); }
    .moves-pane .pane-header { color: var(--moves); }
    .chat-pane .pane-header { color: var(--chat); }
    .pane-body {
      padding: 8px;
      overflow: auto;
      flex: 1;
      white-space: pre;
      line-height: 1.35;
    }
    .board-pane .pane-body {
      display: flex;
      align-items: center;
      justify-content: center;
      padding: 8px;
      overflow: hidden;
    }
    .board-grid {
      display: flex;
      align-items: center;
      justify-content: center;
      width: 100%;
      height: 100%;
      min-width: 0;
      min-height: 0;
      user-select: none;
    }
    .board-square-wrap {
      width: min(100%, calc(100vh - 155px));
      max-width: 100%;
      max-height: 100%;
      aspect-ratio: 1 / 1;
      display: flex;
      align-items: center;
      justify-content: center;
      background: #0f141c;
      border: 1px solid #262c36;
      border-radius: 4px;
      padding: 4px;
    }
    .board-svg {
      width: 100%;
      height: 100%;
      display: block;
    }
    .board-hit {
      cursor: pointer;
    }
    .board-hit:hover .hover-ring {
      opacity: 0.55;
    }
    .hover-ring {
      opacity: 0;
      transition: opacity 0.1s ease;
    }
    .size-btn.active-size {
      border-color: var(--board);
      color: var(--board);
      background: rgba(88, 166, 255, 0.14);
      font-weight: 700;
    }
    .channel-bar {
      display: flex;
      align-items: center;
      gap: 4px;
      padding: 5px 8px;
      border-bottom: 1px solid #21262d;
      background: #0d1219;
      flex-wrap: wrap;
      flex-shrink: 0;
    }
    .ch-pill {
      background: #161b22;
      color: var(--muted);
      border: 1px solid #30363d;
      border-radius: 3px;
      padding: 2px 7px;
      font-family: inherit;
      font-size: 11px;
      cursor: pointer;
      display: inline-flex;
      align-items: center;
      gap: 4px;
    }
    .ch-pill:hover {
      border-color: var(--chat);
      color: var(--text);
    }
    .ch-pill.joined {
      color: #c9d1d9;
    }
    .ch-pill.active {
      border-color: var(--chat);
      color: var(--chat);
      background: rgba(63, 185, 80, 0.14);
      font-weight: 700;
    }
    .ch-unread {
      color: var(--warning);
      font-weight: 700;
      font-size: 10px;
    }
    .channel-directory {
      background: #161b22;
      border-bottom: 1px solid #30363d;
      padding: 8px;
      flex-shrink: 0;
      max-height: 210px;
      overflow-y: auto;
    }
    .channel-dir-header {
      display: flex;
      justify-content: space-between;
      align-items: center;
      margin-bottom: 6px;
      font-weight: 700;
      color: var(--chat);
      font-size: 11px;
    }
    .channel-dir-row {
      display: flex;
      align-items: center;
      justify-content: space-between;
      padding: 4px 6px;
      border-radius: 3px;
      cursor: pointer;
      font-size: 12px;
    }
    .channel-dir-row:hover, .channel-dir-row.selected {
      background: #21262d;
    }
    .channel-dir-actions {
      display: flex;
      gap: 4px;
    }
    .ch-act-btn {
      background: #21262d;
      color: var(--text);
      border: 1px solid #30363d;
      border-radius: 2px;
      padding: 1px 6px;
      font-family: inherit;
      font-size: 10px;
      cursor: pointer;
    }
    .ch-act-btn.enter {
      border-color: var(--chat);
      color: var(--chat);
    }
    .ch-act-btn.leave {
      border-color: var(--error);
      color: var(--error);
    }

    #source-view {
      display: none;
      flex: 1;
      grid-template-columns: 250px 1fr;
      min-height: 0;
    }
    .file-list {
      border-right: 1px solid var(--border-dim);
      overflow-y: auto;
      background: #11161f;
      padding: 6px 0;
    }
    .file-item {
      padding: 5px 12px;
      cursor: pointer;
      color: var(--muted);
      font-size: 12px;
    }
    .file-item:hover, .file-item.active {
      background: #1f2937;
      color: var(--text);
    }
    .code-viewer {
      padding: 12px;
      overflow: auto;
      white-space: pre;
      font-size: 12px;
      line-height: 1.45;
      color: #e6edf3;
    }

    footer {
      flex-shrink: 0;
      padding: 2px 10px 6px;
      background: #090d12;
      border-top: 1px solid #21262d;
    }
    #status-line {
      color: var(--muted);
      font-size: 12px;
      padding: 2px 0;
      white-space: nowrap;
      overflow: hidden;
      text-overflow: ellipsis;
    }
    #input-row {
      display: flex;
      align-items: center;
      gap: 6px;
    }
    #prompt-label {
      color: var(--board);
      font-weight: 700;
    }
    #cmd-input {
      flex: 1;
      background: transparent;
      border: none;
      outline: none;
      color: var(--text);
      font-family: inherit;
      font-size: 13px;
    }
    .move-row { cursor: pointer; padding: 1px 3px; }
    .move-row:hover { background: #1f242c; }
    .move-row.current { color: var(--warning); font-weight: 700; }
    .sys-msg { color: var(--warning); }
    .log-ok { color: var(--chat); }
    .log-warn { color: var(--warning); }
    .log-err { color: var(--error); }
  </style>
</head>
<body>
  <header>
    <div class="brand">
      <span>● ogs-tui 0.1.0</span>
      <span style="color: var(--muted); font-weight: 400;">Zellij TUI &amp; REPL Preview</span>
    </div>
    <div class="quick-actions">
      <button id="btn-size-9" class="quick-btn size-btn active-size" onclick="runCmd('size 9')">9×9</button>
      <button id="btn-size-13" class="quick-btn size-btn" onclick="runCmd('size 13')">13×13</button>
      <button id="btn-size-19" class="quick-btn size-btn" onclick="runCmd('size 19')">19×19</button>
      <button class="quick-btn" onclick="runCmd('preset ko')">演练: 打劫与死眼</button>
      <button class="quick-btn" onclick="runCmd('estimate auto')">estimate auto</button>
      <button class="quick-btn" onclick="runCmd('estimate exit')">estimate exit</button>
      <button class="quick-btn" onclick="runCmd('prev')">prev</button>
      <button class="quick-btn" onclick="runCmd('next')">next</button>
      <button class="quick-btn" onclick="runCmd('sgf save /tmp/demo_game.sgf')">sgf save</button>
      <button class="quick-btn" onclick="runCmd('sgf load /tmp/demo_game.sgf')">sgf load</button>
      <button class="quick-btn" onclick="runCmd('channels')">channels (进入频道)</button>
      <button class="quick-btn" onclick="runCmd('help')">help</button>
    </div>
    <div class="tabs">
      <button id="tab-tui" class="tab-btn active" onclick="switchView('tui')">TUI / REPL</button>
      <button id="tab-src" class="tab-btn" onclick="switchView('src')">Rust Source (/src)</button>
    </div>
  </header>

  <main id="tui-view">
    <section id="pane-board" class="pane board-pane focused" onclick="setFocus('board')">
      <div class="pane-header">
        <span id="board-title">▶ Board · board · 9×9</span>
        <span id="turn-indicator" style="color: var(--muted); font-weight: 400;">Click intersection or type [a10]</span>
      </div>
      <div class="pane-body">
        <div class="board-grid">
          <div id="board-container" class="board-square-wrap"></div>
        </div>
      </div>
    </section>

    <section id="pane-moves" class="pane moves-pane" onclick="setFocus('moves')">
      <div class="pane-header">
        <span id="moves-title">Moves · 0</span>
        <span id="clock-title" style="color: var(--muted); font-weight: 400;"></span>
      </div>
      <div id="moves-body" class="pane-body"></div>
    </section>

    <section id="pane-chat" class="pane chat-pane" onclick="setFocus('chat')">
      <div class="pane-header">
        <span id="chat-title">Chat game 42</span>
        <span id="unread-badge" style="color: var(--warning);"></span>
      </div>
      <div id="channel-bar" class="channel-bar"></div>
      <div id="channel-directory" class="channel-directory" style="display: none;"></div>
      <div id="chat-body" class="pane-body"></div>
    </section>
  </main>

  <section id="source-view">
    <div id="file-list" class="file-list"></div>
    <div id="code-viewer" class="code-viewer">Select a file on the left to inspect its implementation.</div>
  </section>

  <footer>
    <div id="status-line"></div>
    <div id="input-row">
      <span id="prompt-label">ogs[42]&gt;</span>
      <input id="cmd-input" type="text" placeholder="Type OGS command: [c5], play D4, size 9|13|19, pass, preset ko, channels [zh], join ja, chat hi, help..." autofocus />
    </div>
  </footer>

  <script>
    const COLS = ['A','B','C','D','E','F','G','H','J','K','L','M','N','O','P','Q','R','S','T'];
    const DEFAULT_CHANNELS = [
      { id: 'en', topic: 'English World Chat' },
      { id: 'zh', topic: '中文围棋世界频道' },
      { id: 'ja', topic: '日本語囲碁チャンネル' },
      { id: 'ko', topic: '한국어 바둑 채널' },
      { id: 'de', topic: 'Deutsch Go-Kanal' },
      { id: 'fr', topic: 'Salon Go Francophone' },
      { id: 'es', topic: 'Canal de Go en Español' },
      { id: 'ru', topic: 'Русский канал Го' },
      { id: 'pt', topic: 'Canal de Go em Português' },
      { id: 'it', topic: 'Canale Go Italiano' }
    ];

    const state = {
      gameId: 42,
      size: 9,
      black: 'alice',
      white: 'bob',
      blackRank: '3d',
      whiteRank: '4d',
      komi: 5.5,
      rules: 'japanese',
      phase: 'play',
      timeControl: { system: 'byoyomi', mainSecs: 600, periodSecs: 30, periods: 5 },
      blackClock: { main: 582, periods: 5, period: 30 },
      whiteClock: { main: 590, periods: 5, period: 30 },
      setupStones: [],
      initialToPlay: 'B',
      moves: [
        { color: 'B', col: 2, row: 7 }, // C7
        { color: 'W', col: 6, row: 3 }, // G3
        { color: 'B', col: 2, row: 3 }, // C3
        { color: 'W', col: 6, row: 7 }, // G7
        { color: 'B', col: 4, row: 5 }, // E5
        { color: 'W', col: 1, row: 8 }, // B8 (isolated stone for estimate auto demo)
        { color: 'B', col: 0, row: 8 }, // A8
        { color: 'W', col: 6, row: 5 }, // G5
        { color: 'B', col: 1, row: 7 }, // B7
        { color: 'W', col: 5, row: 4 }, // F4
        { color: 'B', col: 2, row: 8 }, // C8
      ],
      viewMove: null,
      estimateActive: false,
      estimateDead: new Set(),
      estimateSummary: null,
      scoringDead: new Set(),
      focus: 'board',
      worldChannel: 'en',
      chatScope: 'game',
      showChannelPicker: false,
      channelPickerIndex: 0,
      availableChannels: DEFAULT_CHANNELS.map(c => c.id),
      channels: {
        'en': { joined: true, unread: 1, topic: 'English World Chat', messages: [
          { kind: 'chat', author: 'nomad', text: 'Anyone up for a 19x19 or 9x9 teaching game?' },
          { kind: 'chat', author: 'kisei_9d', text: "Use 'size 19' or 'channels zh' to switch board sizes or enter channels!" }
        ]},
        'zh': { joined: true, unread: 0, topic: '中文围棋世界频道', messages: [
          { kind: 'chat', author: 'weiqi_fan', text: '欢迎进入 #zh 中文围棋频道！支持 9x9 / 13x13 / 19x19 线条正方形棋盘与十字星位（╋）。' },
          { kind: 'chat', author: 'guoli_9p', text: "在这里直接输入 'chat 你好' 或在聚焦聊天窗时直接打字即可发言。" }
        ]},
        'ja': { joined: false, unread: 0, topic: '日本語囲碁チャンネル', messages: [
          { kind: 'chat', author: 'honinbo', text: '対局よろしくお願いします！ (#ja チャンネル)' }
        ]},
        'ko': { joined: false, unread: 0, topic: '한국어 바둑 채널', messages: [
          { kind: 'chat', author: 'baduk_pro', text: '안녕하세요! #ko 채널에 오신 것을 환영합니다.' }
        ]},
        'de': { joined: false, unread: 0, topic: 'Deutsch Go-Kanal', messages: [
          { kind: 'chat', author: 'berlin_go', text: 'Willkommen im deutschen Go-Kanal (#de)!' }
        ]},
        'fr': { joined: false, unread: 0, topic: 'Salon Go Francophone', messages: [
          { kind: 'chat', author: 'paris_atari', text: 'Bienvenue sur le canal francophone (#fr) !' }
        ]},
        'es': { joined: false, unread: 0, topic: 'Canal de Go en Español', messages: [] },
        'ru': { joined: false, unread: 0, topic: 'Русский канал Го', messages: [] },
        'pt': { joined: false, unread: 0, topic: 'Canal de Go em Português', messages: [] },
        'it': { joined: false, unread: 0, topic: 'Canale Go Italiano', messages: [] },
        'game/42': { joined: true, unread: 0, topic: 'Game #42 alice vs bob', messages: [
          { kind: 'sys', author: '', text: 'loaded Game #42 (9x9, japanese, komi 5.5, byo-yomi 10:00 + 5x0:30)' },
          { kind: 'chat', author: 'alice', text: "glhf! Try 9×9 / 13×13 / 19×19 buttons, 'preset ko', or click 'channels' to enter #zh / #en / #ja." }
        ]}
      },
      logs: [
        { level: 'ok', text: 'Square Line-Grid Board (9×9, 13×13, 19×19) with Cross Hoshi (┼) & Full Go Rules active' }
      ]
    };

    function ensureChannel(chId) {
      const clean = chId.replace(/^#/, '').toLowerCase();
      if (!state.channels[clean]) {
        const meta = DEFAULT_CHANNELS.find(d => d.id === clean);
        state.channels[clean] = {
          joined: false,
          unread: 0,
          topic: meta ? meta.topic : 'World channel #' + clean,
          messages: []
        };
      }
      if (!state.availableChannels.includes(clean)) {
        state.availableChannels.push(clean);
      }
      return clean;
    }

    function enterWorldChannel(rawCh) {
      const ch = ensureChannel(rawCh);
      const wasJoined = state.channels[ch].joined;
      state.channels[ch].joined = true;
      state.channels[ch].unread = 0;
      state.worldChannel = ch;
      state.chatScope = 'world';
      state.focus = 'chat';
      state.showChannelPicker = false;
      if (!wasJoined) {
        state.channels[ch].messages.push({
          kind: 'sys',
          author: '',
          text: \`已加入并进入世界频道 #\${ch} (\${state.channels[ch].topic})\`
        });
      }
      pushLog('ok', \`entered world channel #\${ch} (\${state.channels[ch].topic}) — type 'chat <msg>' or type directly to talk\`, false);
      updateFocusClasses();
      render();
    }

    function leaveWorldChannel(rawCh, evt) {
      if (evt) evt.stopPropagation();
      const ch = ensureChannel(rawCh);
      state.channels[ch].joined = false;
      state.channels[ch].unread = 0;
      if (state.chatScope === 'world' && state.worldChannel === ch) {
        const nextJoined = state.availableChannels.find(id => state.channels[id]?.joined);
        if (nextJoined) {
          state.worldChannel = nextJoined;
        } else {
          state.chatScope = 'game';
        }
      }
      pushLog('ok', \`left world channel #\${ch}\`, false);
      render();
    }

    function switchToGameChat() {
      state.chatScope = 'game';
      state.channels['game/' + state.gameId].unread = 0;
      state.showChannelPicker = false;
      pushLog('ok', \`switched chat scope to Game #\${state.gameId}\`, false);
      render();
    }

    function setBoardSizeModel(newSize) {
      if (![9, 13, 19].includes(newSize)) {
        pushLog('err', \`unsupported board size \${newSize} (supported: 9, 13, 19)\`);
        return;
      }
      state.size = newSize;
      state.komi = newSize === 19 ? 6.5 : 5.5;
      state.setupStones = [];
      state.initialToPlay = 'B';
      state.viewMove = null;
      state.estimateActive = false;
      state.estimateDead.clear();
      state.estimateSummary = null;

      // Seed opening star-point moves appropriate for 9x9, 13x13, or 19x19
      if (newSize === 9) {
        state.moves = [
          { color: 'B', col: 2, row: 7 }, // C7
          { color: 'W', col: 6, row: 3 }, // G3
          { color: 'B', col: 6, row: 7 }, // G7
          { color: 'W', col: 2, row: 3 }, // C3
          { color: 'B', col: 4, row: 5 }, // E5 (Tengen)
        ];
      } else if (newSize === 13) {
        state.moves = [
          { color: 'B', col: 3, row: 10 }, // D10
          { color: 'W', col: 9, row: 4 },  // K4
          { color: 'B', col: 9, row: 10 }, // K10
          { color: 'W', col: 3, row: 4 },  // D4
          { color: 'B', col: 6, row: 7 },  // G7 (Tengen)
        ];
      } else {
        state.moves = [
          { color: 'B', col: 15, row: 16 }, // Q16
          { color: 'W', col: 3, row: 4 },   // D4
          { color: 'B', col: 15, row: 4 },  // Q4
          { color: 'W', col: 3, row: 16 },  // D16
          { color: 'B', col: 9, row: 10 },  // K10 (Tengen)
        ];
      }
      pushLog('ok', \`已切换至 \${newSize}×\${newSize} 正方形线条棋盘（十字星位 ┼，已绑定死眼自杀禁手、单劫禁手 □ 与全局同形 Superko 规则）\`);
    }

    function displayedMoveCount() {
      return state.viewMove === null ? state.moves.length : Math.min(state.viewMove, state.moves.length);
    }

    function isReviewing() {
      return displayedMoveCount() < state.moves.length;
    }

    function gridSignature(grid) {
      return grid.map(row => row.map(cell => cell || '.').join('')).join('/');
    }

    function getNeighbors(size, c, r) {
      const out = [];
      if (c > 0) out.push([c - 1, r]);
      if (c + 1 < size) out.push([c + 1, r]);
      if (r > 1) out.push([c, r - 1]);
      if (r < size) out.push([c, r + 1]);
      return out;
    }

    function getGroupInfo(grid, size, c0, r0) {
      const color = grid[r0 - 1]?.[c0];
      if (!color) return { stones: [], liberties: 0 };
      const seen = new Set([c0 + ',' + r0]);
      const libs = new Set();
      const queue = [[c0, r0]];
      const stones = [];
      while (queue.length) {
        const [c, r] = queue.pop();
        stones.push([c, r]);
        for (const [nc, nr] of getNeighbors(size, c, r)) {
          const cell = grid[nr - 1][nc];
          const key = nc + ',' + nr;
          if (!cell) libs.add(key);
          else if (cell === color && !seen.has(key)) {
            seen.add(key);
            queue.push([nc, nr]);
          }
        }
      }
      return { stones, liberties: libs.size };
    }

    function buildBoardAt(limit) {
      const size = state.size;
      const grid = Array.from({ length: size }, () => Array(size).fill(null));
      for (const s of state.setupStones) {
        if (s.col >= 0 && s.col < size && s.row >= 1 && s.row <= size) {
          grid[s.row - 1][s.col] = s.color;
        }
      }
      const positionHistory = new Set([gridSignature(grid)]);
      let capB = 0, capW = 0;
      let lastMove = null;
      let koPoint = null;
      let toPlay = state.moves.length > 0 && limit === 0
        ? state.moves[0].color
        : state.initialToPlay;

      for (let i = 0; i < limit; i++) {
        const mv = state.moves[i];
        mv.captured = 0;
        if (mv.col === null || mv.row === null) {
          lastMove = null;
          koPoint = null;
          toPlay = mv.color === 'B' ? 'W' : 'B';
          continue;
        }
        if (mv.col < 0 || mv.col >= size || mv.row < 1 || mv.row > size) continue;
        grid[mv.row - 1][mv.col] = mv.color;
        const opp = mv.color === 'B' ? 'W' : 'B';
        const capturedStones = [];
        for (const [nc, nr] of getNeighbors(size, mv.col, mv.row)) {
          if (grid[nr - 1][nc] === opp) {
            const grp = getGroupInfo(grid, size, nc, nr);
            if (grp.liberties === 0) {
              for (const [sc, sr] of grp.stones) {
                grid[sr - 1][sc] = null;
                capturedStones.push([sc, sr]);
              }
            }
          }
        }
        mv.captured = capturedStones.length;
        if (mv.color === 'B') capB += capturedStones.length;
        else capW += capturedStones.length;

        // Simple Ko detection: exactly 1 stone captured AND the capturing stone's group has size 1 and 1 liberty
        if (capturedStones.length === 1) {
          const ownGrp = getGroupInfo(grid, size, mv.col, mv.row);
          if (ownGrp.stones.length === 1 && ownGrp.liberties === 1) {
            koPoint = { col: capturedStones[0][0], row: capturedStones[0][1] };
          } else {
            koPoint = null;
          }
        } else {
          koPoint = null;
        }

        positionHistory.add(gridSignature(grid));
        lastMove = { col: mv.col, row: mv.row };
        toPlay = mv.color === 'B' ? 'W' : 'B';
      }
      return { grid, capB, capW, lastMove, koPoint, positionHistory, toPlay };
    }

    function validateGoMove(grid, col, row, color, koPoint, positionHistory) {
      const size = state.size;
      if (col < 0 || col >= size || row < 1 || row > size) {
        return { ok: false, code: 'OffBoard', reason: \`坐标超出 \${size}×\${size} 棋盘范围 (OffBoard)\` };
      }
      if (grid[row - 1][col] !== null) {
        return { ok: false, code: 'Occupied', reason: '已有棋子，不可落子 (Position already occupied)' };
      }
      if (koPoint && koPoint.col === col && koPoint.row === row) {
        return {
          ok: false,
          code: 'Ko',
          reason: \`打劫禁手：刚提之眼 [\${toGtp(col, row)}] 不可立即回提，需先在别处寻劫 (Ko rule violation)\`
        };
      }

      // Clone board and simulate capture-first order
      const simGrid = grid.map(r => [...r]);
      simGrid[row - 1][col] = color;
      const opp = color === 'B' ? 'W' : 'B';
      const captured = [];

      for (const [nc, nr] of getNeighbors(size, col, row)) {
        if (simGrid[nr - 1][nc] === opp) {
          const grp = getGroupInfo(simGrid, size, nc, nr);
          if (grp.liberties === 0) {
            for (const [sc, sr] of grp.stones) {
              simGrid[sr - 1][sc] = null;
              captured.push([sc, sr]);
            }
          }
        }
      }

      // Check own connected group liberties after opponent 0-liberty stones are removed
      const ownGrp = getGroupInfo(simGrid, size, col, row);
      if (ownGrp.liberties === 0) {
        return {
          ok: false,
          code: 'Suicide',
          reason: \`禁入点：死眼无气自杀禁手 [\${toGtp(col, row)}]，不可落子 (Suicide move into dead eye)\`
        };
      }

      // Positional Superko check
      const sig = gridSignature(simGrid);
      if (positionHistory && positionHistory.has(sig)) {
        return {
          ok: false,
          code: 'Superko',
          reason: \`全局同形禁手：在 [\${toGtp(col, row)}] 落子将重复历史盘面 (Positional Superko violation)\`
        };
      }

      let nextKo = null;
      if (captured.length === 1 && ownGrp.stones.length === 1 && ownGrp.liberties === 1) {
        nextKo = { col: captured[0][0], row: captured[0][1] };
      }

      return { ok: true, capturedCount: captured.length, nextKo };
    }

    function connectedStoneGroup(grid, c0, r0) {
      return getGroupInfo(grid, state.size, c0, r0).stones;
    }

    function computeEstimate(grid, capB, capW) {
      const size = state.size;
      const effective = Array.from({ length: size }, (_, rIdx) =>
        Array.from({ length: size }, (_, cIdx) => {
          const key = cIdx + ',' + (rIdx + 1);
          if (state.estimateDead.has(key)) return null;
          return grid[rIdx][cIdx];
        })
      );

      const territory = Array.from({ length: size }, () => Array(size).fill(null));
      const visited = new Set();

      for (let r = 1; r <= size; r++) {
        for (let c = 0; c < size; c++) {
          const key = c + ',' + r;
          if (effective[r - 1][c] !== null || visited.has(key)) continue;
          const region = [];
          const queue = [[c, r]];
          visited.add(key);
          let touchesB = false, touchesW = false;
          while (queue.length) {
            const [cc, rr] = queue.pop();
            region.push([cc, rr]);
            for (const [nc, nr] of getNeighbors(size, cc, rr)) {
              const nColor = effective[nr - 1][nc];
              if (nColor === 'B') touchesB = true;
              else if (nColor === 'W') touchesW = true;
              else {
                const nKey = nc + ',' + nr;
                if (!visited.has(nKey)) {
                  visited.add(nKey);
                  queue.push([nc, nr]);
                }
              }
            }
          }
          const owner = touchesB && !touchesW ? 'B' : touchesW && !touchesB ? 'W' : null;
          for (const [rc, rr] of region) {
            territory[rr - 1][rc] = owner;
          }
        }
      }

      let bTerr = 0, wTerr = 0, bStones = 0, wStones = 0, deadB = 0, deadW = 0;
      for (let r = 1; r <= size; r++) {
        for (let c = 0; c < size; c++) {
          const orig = grid[r - 1][c];
          const dead = state.estimateDead.has(c + ',' + r);
          if (orig === 'B' && dead) deadB++;
          if (orig === 'W' && dead) deadW++;
          if (effective[r - 1][c] === 'B') bStones++;
          if (effective[r - 1][c] === 'W') wStones++;
          if (territory[r - 1][c] === 'B') bTerr++;
          if (territory[r - 1][c] === 'W') wTerr++;
        }
      }

      const isArea = state.rules === 'chinese' || state.rules === 'aga';
      const blackTotal = isArea ? bStones + bTerr : bTerr + capB + deadW;
      const whiteTotal = isArea ? wStones + wTerr + state.komi : wTerr + capW + deadB + state.komi;
      const diff = blackTotal - whiteTotal;
      const result = diff > 0 ? \`B+\${diff.toFixed(1)}\` : diff < 0 ? \`W+\${Math.abs(diff).toFixed(1)}\` : 'Jigo';

      return { territory, summary: \`\${result} (B \${blackTotal.toFixed(1)} vs W \${whiteTotal.toFixed(1)}, \${state.rules})\` };
    }

    function autoDetectDead(grid) {
      const size = state.size;
      const dead = new Set();
      const seen = new Set();
      for (let r = 1; r <= size; r++) {
        for (let c = 0; c < size; c++) {
          const color = grid[r - 1][c];
          const key = c + ',' + r;
          if (!color || seen.has(key)) continue;
          const group = connectedStoneGroup(grid, c, r);
          group.forEach(([gc, gr]) => seen.add(gc + ',' + gr));
          const liberties = new Set();
          let enemyNeighbors = 0;
          for (const [gc, gr] of group) {
            for (const [nc, nr] of getNeighbors(size, gc, gr)) {
              const cell = grid[nr - 1][nc];
              if (!cell) liberties.add(nc + ',' + nr);
              else if (cell !== color) enemyNeighbors++;
            }
          }
          if (group.length <= 2 && liberties.size <= 1 && enemyNeighbors >= 2) {
            group.forEach(([gc, gr]) => dead.add(gc + ',' + gr));
          }
        }
      }
      return dead;
    }

    function parseCoordToken(raw) {
      const cleaned = raw.trim().replace(/^\\[|\\]$/g, '').replace(/\\s+/g, '').toUpperCase();
      if (cleaned.length < 2) return null;
      const letter = cleaned[0];
      if (letter === 'I') return { error: 'column letter I is skipped in Go notation' };
      const col = COLS.indexOf(letter);
      const row = Number(cleaned.slice(1));
      if (col < 0 || col >= state.size || !Number.isInteger(row) || row < 1 || row > state.size) {
        return { error: \`coordinate out of \${state.size}x\${state.size} bounds: \${raw}\` };
      }
      return { col, row };
    }

    function toGtp(col, row) {
      return COLS[col] + String(row);
    }

    function pushLog(level, text, mirrorToChat = true) {
      state.logs.push({ level, text });
      if (mirrorToChat) {
        const ch = state.chatScope === 'game' ? 'game/' + state.gameId : state.worldChannel;
        if (state.channels[ch]) {
          state.channels[ch].messages.push({ kind: 'sys', author: '', text });
        }
      }
    }

    function handleBoardClick(col, row) {
      runCmd(\`[\${COLS[col].toLowerCase()}\${row}]\`);
    }

    function loadKoAndEyePreset() {
      const topRow = state.size;
      const rightCol = state.size - 1;
      state.estimateActive = false;
      state.estimateDead.clear();
      state.viewMove = null;
      state.initialToPlay = 'W';
      // Adapts automatically to current board size (9x9, 13x13, or 19x19):
      // 1) Ko shape around B2 / C2
      // 2) Single-stone Dead Eye at bottom-right corner (rightCol, 1)
      // 3) Multi-stone Dead Eye at top-left corner (A{topRow})
      state.setupStones = [
        { color: 'B', col: 0, row: 3 },
        { color: 'W', col: 1, row: 3 },
        { color: 'B', col: 2, row: 3 },
        { color: 'W', col: 0, row: 2 },
        { color: 'B', col: 1, row: 2 },
        { color: 'B', col: 3, row: 2 },
        { color: 'B', col: 0, row: 1 },
        { color: 'W', col: 1, row: 1 },
        { color: 'B', col: 2, row: 1 },
        // White surround around bottom-right corner — dead eye for Black
        { color: 'W', col: rightCol - 1, row: 1 },
        { color: 'W', col: rightCol, row: 2 },
        // Multi-stone dead eye at top-left corner A{topRow} for Black
        { color: 'B', col: 1, row: topRow },
        { color: 'W', col: 2, row: topRow },
        { color: 'W', col: 1, row: topRow - 1 },
        { color: 'W', col: 0, row: topRow - 1 },
      ];
      state.moves = [
        { color: 'W', col: 2, row: 2 } // White plays C2, capturing Black at B2 -> B2 is now Ko (□)!
      ];
      const brCorner = toGtp(rightCol, 1);
      const tlCorner = toGtp(0, topRow);
      pushLog(
        'ok',
        \`已加载 \${state.size}×\${state.size}【打劫与死眼自杀校验】盘面（轮到黑棋 B）：1) B2(□) 为刚被白 C2 提掉的劫眼，点击 B2 触发【打劫禁手】；2) \${brCorner} 与 \${tlCorner} 为黑方无气死眼，点击触发【死眼自杀禁手】；3) 先在别处寻劫后方可提回 B2。\`
      );
    }

    async function runCmd(rawLine) {
      const line = rawLine.trim();
      if (!line) return;
      document.getElementById('cmd-input').value = '';

      if (line.startsWith('[')) {
        const parsed = parseCoordToken(line);
        if (parsed.error) {
          pushLog('err', parsed.error);
          render();
          return;
        }
        applyCoordAction(parsed.col, parsed.row);
        render();
        return;
      }

      const parts = line.split(/\\s+/);
      const kw = parts[0].toLowerCase();

      if (kw === 'play' || kw === 'move') {
        const parsed = parseCoordToken(parts[1] || '');
        if (!parsed || parsed.error) {
          pushLog('err', parsed?.error || 'usage: play <coord>');
        } else {
          applyCoordAction(parsed.col, parsed.row);
        }
      } else if (kw === 'size' || kw === 'board' || kw === 'new') {
        const sz = Number((parts[1] || '').replace(/x.*$/i, ''));
        setBoardSizeModel(sz);
      } else if (kw === 'preset') {
        loadKoAndEyePreset();
      } else if (kw === 'pass') {
        if (isReviewing()) {
          state.moves = state.moves.slice(0, displayedMoveCount());
          state.viewMove = null;
        }
        const { toPlay } = buildBoardAt(displayedMoveCount());
        state.moves.push({ color: toPlay, col: null, row: null });
        state.viewMove = null;
        pushLog('ok', \`game \${state.gameId}: \${toPlay} passed (ko cleared)\`);
      } else if (kw === 'estimate' || kw === 'est') {
        const sub = (parts[1] || '').toLowerCase();
        if (!sub) {
          state.estimateActive = true;
          pushLog('ok', "estimate mode entered: click stone or type [a10] to toggle dead groups, 'estimate auto' for Monte-Carlo");
        } else if (sub === 'auto') {
          const { grid, capB, capW } = buildBoardAt(displayedMoveCount());
          state.estimateActive = true;
          state.estimateDead = autoDetectDead(grid);
          const est = computeEstimate(grid, capB, capW);
          state.estimateSummary = est.summary;
          pushLog('ok', \`Monte-Carlo auto estimate: \${est.summary} (\${state.estimateDead.size} dead stone(s) detected)\`);
        } else if (sub === 'confirm' || sub === 'score') {
          state.estimateActive = true;
          const { grid, capB, capW } = buildBoardAt(displayedMoveCount());
          const est = computeEstimate(grid, capB, capW);
          state.estimateSummary = est.summary;
          pushLog('ok', \`local estimate confirmed: \${est.summary} (informational only; official result uses dead/dame + score accept)\`);
        } else if (sub === 'exit' || sub === 'off') {
          state.estimateActive = false;
          state.estimateDead.clear();
          state.estimateSummary = null;
          pushLog('ok', 'estimate mode exited');
        }
      } else if (kw === 'prev' || kw === 'back') {
        const n = Math.max(1, Number(parts[1] || 1));
        state.viewMove = Math.max(0, displayedMoveCount() - n);
        pushLog('ok', \`reviewing move \${state.viewMove}/\${state.moves.length}\`);
      } else if (kw === 'next' || kw === 'forward') {
        const n = Math.max(1, Number(parts[1] || 1));
        const target = Math.min(state.moves.length, displayedMoveCount() + n);
        state.viewMove = target >= state.moves.length ? null : target;
        pushLog('ok', state.viewMove === null ? \`at latest move \${state.moves.length}\` : \`reviewing move \${state.viewMove}/\${state.moves.length}\`);
      } else if (kw === 'first' || kw === 'start') {
        state.viewMove = 0;
        pushLog('ok', \`reviewing move 0/\${state.moves.length}\`);
      } else if (kw === 'last' || kw === 'end' || kw === 'live') {
        state.viewMove = null;
        pushLog('ok', \`returned to live move \${state.moves.length}\`);
      } else if (kw === 'goto' || kw === 'step') {
        const arg = (parts[1] || 'next').toLowerCase();
        if (arg === 'prev') return runCmd('prev ' + (parts[2] || '1'));
        if (arg === 'next') return runCmd('next ' + (parts[2] || '1'));
        if (arg === 'first') return runCmd('first');
        if (arg === 'last') return runCmd('last');
        const num = Number(arg);
        if (Number.isInteger(num) && num >= 0) {
          state.viewMove = num >= state.moves.length ? null : num;
          pushLog('ok', \`jumped to move \${displayedMoveCount()}/\${state.moves.length}\`);
        }
      } else if (kw === 'sgf') {
        const sub = (parts[1] || '').toLowerCase();
        const filePath = parts.slice(2).join(' ') || '/tmp/demo_game.sgf';
        if (sub === 'save') {
          const sgfText = buildSgfString();
          const res = await fetch('/api/sgf/save', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ path: filePath, content: sgfText })
          }).then(r => r.json());
          if (res.ok) pushLog('ok', \`saved game \${state.gameId} SGF to '\${filePath}'\`);
          else pushLog('err', res.error || 'save failed');
        } else if (sub === 'load') {
          const res = await fetch('/api/sgf/load?path=' + encodeURIComponent(filePath)).then(r => r.json());
          if (res.ok) {
            loadFromSgfString(res.content, filePath);
          } else {
            pushLog('err', res.error || 'load failed');
          }
        } else {
          pushLog('warn', 'usage: sgf load <path> | sgf save <path> | sgf <game_id>');
        }
      } else if (kw === 'channels' || kw === 'channel') {
        const arg1 = (parts[1] || '').toLowerCase();
        const arg2 = (parts[2] || '').toLowerCase();
        const target = (arg1 === 'join' || arg1 === 'enter') ? arg2 : arg1;
        if (target) {
          enterWorldChannel(target);
          return;
        }
        state.showChannelPicker = !state.showChannelPicker;
        state.focus = 'chat';
        updateFocusClasses();
        const joinedList = state.availableChannels.filter(c => state.channels[c]?.joined).map(c => '#' + c + '(已加入)').join(', ');
        pushLog(
          'ok',
          \`频道列表已打开（点击频道即可进入，或输入 'channels <名>' / 'join <名>' 进入）：\${joinedList || 'none'} | 全部: \${state.availableChannels.map(c => '#' + c).join(', ')}\`,
          false
        );
      } else if (kw === 'join' || kw === 'world') {
        const ch = (parts[1] || 'en').replace(/^#/, '');
        enterWorldChannel(ch);
        return;
      } else if (kw === 'part' || kw === 'leave') {
        const ch = (parts[1] || state.worldChannel).replace(/^#/, '');
        leaveWorldChannel(ch);
        return;
      } else if (kw === 'game' || kw === 'g') {
        switchToGameChat();
        return;
      } else if (kw === 'chat' || kw === 'say') {
        const text = line.slice(parts[0].length).trim();
        if (!text) {
          pushLog('warn', 'usage: chat <message>');
        } else {
          const ch = state.chatScope === 'game' ? 'game/' + state.gameId : state.worldChannel;
          ensureChannel(ch);
          state.channels[ch].messages.push({ kind: 'chat', author: 'you', text });
          pushLog('ok', \`sent message to \${state.chatScope === 'game' ? 'Game #' + state.gameId : '#' + state.worldChannel}\`, false);
        }
      } else if (kw === 'help' || kw === '?') {
        pushLog('ok', 'Commands: [a10] | play D4 | size 9|13|19 | pass | preset ko | estimate [auto|confirm|exit] | step [prev|next|first|last|<n>] | channels [<ch>] | join <ch> | part <ch> | world <ch> | chat <msg>');
      } else if (state.focus === 'chat' || state.chatScope === 'world') {
        // When focused on chat or inside a world channel, plain text sends a chat message directly
        const ch = state.chatScope === 'game' ? 'game/' + state.gameId : state.worldChannel;
        ensureChannel(ch);
        state.channels[ch].messages.push({ kind: 'chat', author: 'you', text: line });
        pushLog('ok', \`sent message to \${state.chatScope === 'game' ? 'Game #' + state.gameId : '#' + state.worldChannel}\`, false);
      } else {
        pushLog('warn', \`command '\${line}' dispatched to SessionActor\`);
      }

      render();
    }

    function applyCoordAction(col, row) {
      const shown = displayedMoveCount();
      const { grid, capB, capW, koPoint, positionHistory, toPlay } = buildBoardAt(shown);
      if (state.estimateActive) {
        const group = connectedStoneGroup(grid, col, row);
        if (!group.length) {
          pushLog('warn', \`no stone at \${toGtp(col, row)}\`);
          return;
        }
        const allDead = group.every(([c, r]) => state.estimateDead.has(c + ',' + r));
        for (const [c, r] of group) {
          const k = c + ',' + r;
          if (allDead) state.estimateDead.delete(k);
          else state.estimateDead.add(k);
        }
        const est = computeEstimate(grid, capB, capW);
        state.estimateSummary = est.summary;
        pushLog('ok', \`toggled group at \${toGtp(col, row)} (\${state.estimateDead.size} dead) — local: \${est.summary}\`);
      } else {
        const check = validateGoMove(grid, col, row, toPlay, koPoint, positionHistory);
        if (!check.ok) {
          pushLog('err', \`非法落子 [\${toGtp(col, row)}]: \${check.reason}\`);
          return;
        }
        if (isReviewing()) {
          state.moves = state.moves.slice(0, shown);
          state.viewMove = null;
        }
        state.moves.push({ color: toPlay, col, row });
        const capInfo = check.capturedCount > 0 ? \` (提子 ×\${check.capturedCount})\` : '';
        const koInfo = check.nextKo ? \` [形成打劫禁入点: \${toGtp(check.nextKo.col, check.nextKo.row)}]\` : '';
        pushLog('ok', \`game \${state.gameId} (\${state.size}×\${state.size}): \${toPlay} played \${toGtp(col, row)}\${capInfo}\${koInfo}\`);
      }
    }

    function buildSgfString() {
      let out = \`(;FF[4]GM[1]SZ[\${state.size}]KM[\${state.komi}]RU[\${state.rules}]PB[\${state.black}]PW[\${state.white}]BR[\${state.blackRank}]WR[\${state.whiteRank}]GN[OGS Game #\${state.gameId}]TM[600]OT[5x30 byo-yomi]\`;
      const ab = state.setupStones.filter(s => s.color === 'B');
      const aw = state.setupStones.filter(s => s.color === 'W');
      if (ab.length) {
        out += 'AB' + ab.map(s => \`[\${String.fromCharCode(97 + s.col)}\${String.fromCharCode(97 + (state.size - s.row))}]\`).join('');
      }
      if (aw.length) {
        out += 'AW' + aw.map(s => \`[\${String.fromCharCode(97 + s.col)}\${String.fromCharCode(97 + (state.size - s.row))}]\`).join('');
      }
      for (const mv of state.moves) {
        if (mv.col === null || mv.row === null) {
          out += \`;\${mv.color}[]\`;
        } else {
          const c = String.fromCharCode(97 + mv.col);
          const r = String.fromCharCode(97 + (state.size - mv.row));
          out += \`;\${mv.color}[\${c}\${r}]\`;
        }
      }
      out += ')';
      return out;
    }

    function loadFromSgfString(sgf, pathLabel) {
      const szMatch = sgf.match(/SZ\\[(\\d+)\\]/);
      if (szMatch) {
        const parsedSz = Number(szMatch[1]);
        if ([9, 13, 19].includes(parsedSz)) state.size = parsedSz;
      }
      const pbMatch = sgf.match(/PB\\[([^\\]]*)\\]/);
      if (pbMatch) state.black = pbMatch[1];
      const pwMatch = sgf.match(/PW\\[([^\\]]*)\\]/);
      if (pwMatch) state.white = pwMatch[1];
      state.setupStones = [];
      const moves = [];
      const re = /;([BW])\\[([a-z]{0,2})\\]/g;
      let m;
      while ((m = re.exec(sgf)) !== null) {
        const color = m[1];
        const raw = m[2];
        if (!raw) {
          moves.push({ color, col: null, row: null });
        } else {
          const col = raw.charCodeAt(0) - 97;
          const rowTop = raw.charCodeAt(1) - 97;
          moves.push({ color, col, row: state.size - rowTop });
        }
      }
      state.moves = moves;
      state.initialToPlay = moves[0]?.color || 'B';
      state.viewMove = null;
      state.estimateActive = false;
      state.estimateDead.clear();
      pushLog('ok', \`loaded SGF '\${pathLabel}' (\${state.size}×\${state.size}, \${moves.length} moves)\`);
    }

    function isHoshi(size, col, row) {
      const rowIdx = size - row;
      if (size === 9) {
        return ((col === 2 || col === 6) && (rowIdx === 2 || rowIdx === 6)) || (col === 4 && rowIdx === 4);
      }
      if (size === 13) {
        return ((col === 3 || col === 9) && (rowIdx === 3 || rowIdx === 9)) || (col === 6 && rowIdx === 6);
      }
      if (size === 19) {
        const pts = [3, 9, 15];
        return pts.includes(col) && pts.includes(rowIdx);
      }
      return false;
    }

    function buildLineGridBoardSvg(size, grid, lastMove, koPoint, est, toPlay) {
      const V = 1000;
      const pad = size === 19 ? 46 : size === 13 ? 56 : 66;
      const span = V - pad * 2;
      const step = size > 1 ? span / (size - 1) : span;
      // Scale stone radius proportionally with grid step (no fixed pixel cap) so stones always match board scale
      const stoneR = step * 0.475;
      const crossArm = Math.max(7, step * 0.18);
      const fontSize = size === 19 ? 20 : size === 13 ? 24 : 28;

      let svg = \`<svg class="board-svg" viewBox="0 0 \${V} \${V}" xmlns="http://www.w3.org/2000/svg">\`;
      svg += \`<defs>
        <radialGradient id="blackStoneGrad" cx="35%" cy="32%" r="65%">
          <stop offset="0%" stop-color="#388bfd" />
          <stop offset="55%" stop-color="#1f6feb" />
          <stop offset="100%" stop-color="#0d2d6c" />
        </radialGradient>
        <radialGradient id="whiteStoneGrad" cx="35%" cy="32%" r="65%">
          <stop offset="0%" stop-color="#ffffff" />
          <stop offset="70%" stop-color="#e6edf3" />
          <stop offset="100%" stop-color="#b1bac4" />
        </radialGradient>
      </defs>\`;
      // Subtle board surface
      svg += \`<rect x="0" y="0" width="\${V}" height="\${V}" rx="8" fill="#0f141c" />\`;

      // Coordinate labels (Top, Bottom, Left, Right)
      for (let c = 0; c < size; c++) {
        const x = (pad + c * step).toFixed(2);
        const lbl = COLS[c];
        svg += \`<text x="\${x}" y="\${(pad * 0.42).toFixed(2)}" fill="#8b949e" font-weight="600" font-size="\${fontSize}" text-anchor="middle" dominant-baseline="central">\${lbl}</text>\`;
        svg += \`<text x="\${x}" y="\${(V - pad * 0.42).toFixed(2)}" fill="#8b949e" font-weight="600" font-size="\${fontSize}" text-anchor="middle" dominant-baseline="central">\${lbl}</text>\`;
      }
      for (let r = 1; r <= size; r++) {
        const rIdx = size - r;
        const y = (pad + rIdx * step).toFixed(2);
        svg += \`<text x="\${(pad * 0.40).toFixed(2)}" y="\${y}" fill="#8b949e" font-weight="600" font-size="\${fontSize}" text-anchor="middle" dominant-baseline="central">\${r}</text>\`;
        svg += \`<text x="\${(V - pad * 0.40).toFixed(2)}" y="\${y}" fill="#8b949e" font-weight="600" font-size="\${fontSize}" text-anchor="middle" dominant-baseline="central">\${r}</text>\`;
      }

      // Square Grid Lines (Horizontal & Vertical lines)
      const start = pad.toFixed(2);
      const end = (V - pad).toFixed(2);
      for (let i = 0; i < size; i++) {
        const pos = (pad + i * step).toFixed(2);
        const isBorder = i === 0 || i === size - 1;
        const strokeColor = isBorder ? '#8b949e' : '#3b434d';
        const strokeW = isBorder ? '2.8' : (size === 19 ? '1.7' : '2.0');
        // Horizontal line
        svg += \`<line x1="\${start}" y1="\${pos}" x2="\${end}" y2="\${pos}" stroke="\${strokeColor}" stroke-width="\${strokeW}" />\`;
        // Vertical line
        svg += \`<line x1="\${pos}" y1="\${start}" x2="\${pos}" y2="\${end}" stroke="\${strokeColor}" stroke-width="\${strokeW}" />\`;
      }

      // Star points (Hoshi) marked with Cross (┼ / ╋)
      for (let r = 1; r <= size; r++) {
        const rIdx = size - r;
        const y = pad + rIdx * step;
        for (let c = 0; c < size; c++) {
          if (!isHoshi(size, c, r)) continue;
          const x = pad + c * step;
          svg += \`<g class="hoshi-cross" stroke="#d29922" stroke-width="3.4" stroke-linecap="square">\`;
          svg += \`<line x1="\${(x - crossArm).toFixed(2)}" y1="\${y.toFixed(2)}" x2="\${(x + crossArm).toFixed(2)}" y2="\${y.toFixed(2)}" />\`;
          svg += \`<line x1="\${x.toFixed(2)}" y1="\${(y - crossArm).toFixed(2)}" x2="\${x.toFixed(2)}" y2="\${(y + crossArm).toFixed(2)}" />\`;
          svg += \`</g>\`;
        }
      }

      // Stones, Ko marker (□), Territory, Last move ring, and interactive click targets
      for (let r = size; r >= 1; r--) {
        const rIdx = size - r;
        const y = pad + rIdx * step;
        for (let c = 0; c < size; c++) {
          const x = pad + c * step;
          const stone = grid[r - 1][c];
          const key = c + ',' + r;
          const isDead = state.estimateActive && state.estimateDead.has(key);
          const isLast = lastMove && lastMove.col === c && lastMove.row === r;
          const isKo = !stone && !state.estimateActive && koPoint && koPoint.col === c && koPoint.row === r;
          const star = isHoshi(size, c, r);
          const coordCode = COLS[c].toLowerCase() + r;
          let tip = \`[\${coordCode}] (\${toGtp(c, r)})\${star ? ' ★星位(┼)' : ''}\`;
          if (isKo) tip = \`[\${coordCode}] 打劫禁入点 (Ko point □)\`;

          svg += \`<g class="board-hit" onclick="handleBoardClick(\${c}, \${r})"><title>\${tip}</title>\`;

          if (stone === 'B') {
            svg += \`<circle cx="\${x.toFixed(2)}" cy="\${y.toFixed(2)}" r="\${stoneR.toFixed(2)}" fill="url(#blackStoneGrad)" stroke="#58a6ff" stroke-width="\${(Math.max(2, stoneR * 0.08)).toFixed(2)}" opacity="\${isDead ? '0.42' : '1'}" />\`;
          } else if (stone === 'W') {
            svg += \`<circle cx="\${x.toFixed(2)}" cy="\${y.toFixed(2)}" r="\${stoneR.toFixed(2)}" fill="url(#whiteStoneGrad)" stroke="#8b949e" stroke-width="\${(Math.max(2, stoneR * 0.07)).toFixed(2)}" opacity="\${isDead ? '0.42' : '1'}" />\`;
          } else if (isKo) {
            const koHalf = stoneR * 0.58;
            svg += \`<rect x="\${(x - koHalf).toFixed(2)}" y="\${(y - koHalf).toFixed(2)}" width="\${(koHalf * 2).toFixed(2)}" height="\${(koHalf * 2).toFixed(2)}" fill="rgba(248, 81, 73, 0.22)" stroke="#e3b341" stroke-width="2.8" />\`;
          } else if (est && est.territory[r - 1][c] === 'B') {
            const tHalf = stoneR * 0.36;
            svg += \`<rect x="\${(x - tHalf).toFixed(2)}" y="\${(y - tHalf).toFixed(2)}" width="\${(tHalf * 2).toFixed(2)}" height="\${(tHalf * 2).toFixed(2)}" fill="#388bfd" />\`;
          } else if (est && est.territory[r - 1][c] === 'W') {
            const tHalf = stoneR * 0.36;
            svg += \`<rect x="\${(x - tHalf).toFixed(2)}" y="\${(y - tHalf).toFixed(2)}" width="\${(tHalf * 2).toFixed(2)}" height="\${(tHalf * 2).toFixed(2)}" fill="#d2a8ff" />\`;
          } else {
            // Hover preview ring on empty intersection
            const hoverStroke = toPlay === 'B' ? '#58a6ff' : '#f0f6fc';
            svg += \`<circle class="hover-ring" cx="\${x.toFixed(2)}" cy="\${y.toFixed(2)}" r="\${(stoneR * 0.78).toFixed(2)}" fill="none" stroke="\${hoverStroke}" stroke-width="2.2" stroke-dasharray="4 3" />\`;
          }

          if (isDead && stone) {
            const dArm = stoneR * 0.56;
            svg += \`<line x1="\${(x - dArm).toFixed(2)}" y1="\${(y - dArm).toFixed(2)}" x2="\${(x + dArm).toFixed(2)}" y2="\${(y + dArm).toFixed(2)}" stroke="#f85149" stroke-width="3.4" />\`;
            svg += \`<line x1="\${(x + dArm).toFixed(2)}" y1="\${(y - dArm).toFixed(2)}" x2="\${(x - dArm).toFixed(2)}" y2="\${(y + dArm).toFixed(2)}" stroke="#f85149" stroke-width="3.4" />\`;
          }

          if (isLast && stone) {
            const ringColor = stone === 'B' ? '#f0f6fc' : '#1f6feb';
            svg += \`<circle cx="\${x.toFixed(2)}" cy="\${y.toFixed(2)}" r="\${(stoneR * 0.42).toFixed(2)}" fill="none" stroke="\${ringColor}" stroke-width="\${(Math.max(2.6, stoneR * 0.11)).toFixed(2)}" />\`;
          }

          // Invisible hit target covering the intersection square
          svg += \`<rect x="\${(x - step * 0.48).toFixed(2)}" y="\${(y - step * 0.48).toFixed(2)}" width="\${(step * 0.96).toFixed(2)}" height="\${(step * 0.96).toFixed(2)}" fill="transparent" />\`;
          svg += \`</g>\`;
        }
      }

      svg += '</svg>';
      return svg;
    }

    function render() {
      const shownMoves = displayedMoveCount();
      const { grid, capB, capW, lastMove, koPoint, toPlay } = buildBoardAt(shownMoves);
      const est = state.estimateActive ? computeEstimate(grid, capB, capW) : null;
      if (est) state.estimateSummary = est.summary;

      // Update size buttons active state
      [9, 13, 19].forEach(sz => {
        const btn = document.getElementById('btn-size-' + sz);
        if (btn) btn.classList.toggle('active-size', state.size === sz);
      });

      const modeLabel = state.estimateActive ? 'estimate' : isReviewing() ? 'review' : 'board';
      document.getElementById('board-title').textContent =
        (state.focus === 'board' ? '▶ ' : '') + \`Board · \${modeLabel} · \${state.size}×\${state.size}\`;
      const koBadge = koPoint ? \` · 劫点 Ko: \${toGtp(koPoint.col, koPoint.row)} (□)\` : '';
      document.getElementById('turn-indicator').textContent =
        \`Turn: \${toPlay === 'B' ? '● Black' : '○ White'}\${koBadge}\`;

      document.getElementById('board-container').innerHTML =
        buildLineGridBoardSvg(state.size, grid, lastMove, koPoint, est, toPlay);

      // Moves pane
      document.getElementById('moves-title').textContent =
        (state.focus === 'moves' ? '▶ ' : '') + \`Moves · \${shownMoves}/\${state.moves.length}\`;
      document.getElementById('clock-title').textContent =
        \`B 09:42 (5x30s) · W 09:50 (5x30s)\`;

      let movesHtml = \`<div><strong>\${state.black} (\${state.blackRank}) vs \${state.white} (\${state.whiteRank})</strong></div>\`;
      movesHtml += \`<div style="color:var(--muted)">size \${state.size}×\${state.size} · rules \${state.rules} · komi \${state.komi} · captures B \${capB} · W \${capW}</div>\`;
      if (koPoint) {
        movesHtml += \`<div style="color:var(--warning)">KO FORBIDDEN POINT: \${toGtp(koPoint.col, koPoint.row)} (□ 刚提之眼不可立即回提)</div>\`;
      }
      if (state.estimateActive && state.estimateSummary) {
        movesHtml += \`<div style="color:var(--warning)">LOCAL ESTIMATE: \${state.estimateSummary}</div>\`;
      }
      if (isReviewing()) {
        movesHtml += \`<div style="color:var(--warning)">REVIEW CURSOR: move \${shownMoves}/\${state.moves.length} (click board to branch from here)</div>\`;
      }
      movesHtml += '<hr style="border:none;border-top:1px solid #21262d;margin:6px 0"/>';
      state.moves.forEach((mv, idx) => {
        const num = idx + 1;
        const coordText = mv.col === null ? 'pass' : toGtp(mv.col, mv.row);
        const capText = mv.captured ? \` ×\${mv.captured}\` : '';
        const curCls = num === shownMoves ? 'move-row current' : 'move-row';
        movesHtml += \`<div class="\${curCls}" onclick="runCmd('goto \${num}')">\${String(num).padStart(3, ' ')} \${mv.color} \${coordText}\${capText}</div>\`;
      });
      document.getElementById('moves-body').innerHTML = movesHtml;

      // Chat pane & Channel bar
      const chKey = state.chatScope === 'game' ? 'game/' + state.gameId : state.worldChannel;
      const chObj = state.channels[chKey] || { messages: [] };
      document.getElementById('chat-title').textContent =
        (state.focus === 'chat' ? '▶ ' : '') + (state.chatScope === 'game' ? \`Chat game \${state.gameId}\` : \`Chat #\${state.worldChannel}\`);
      const totalUnread = Object.values(state.channels).reduce((acc, c) => acc + (c.unread || 0), 0);
      document.getElementById('unread-badge').textContent = totalUnread > 0 ? \`unread \${totalUnread}\` : '';

      // Render clickable channel pills bar (Game #42 + joined/quick channels + Directory toggle)
      let barHtml = \`<button class="ch-pill \${state.chatScope === 'game' ? 'active' : 'joined'}" onclick="event.stopPropagation(); switchToGameChat()">Game #\${state.gameId}</button>\`;
      for (const chId of state.availableChannels) {
        const info = state.channels[chId] || { joined: false, unread: 0 };
        const isActive = state.chatScope === 'world' && state.worldChannel === chId;
        if (info.joined || isActive || ['en', 'zh', 'ja', 'ko'].includes(chId)) {
          const dot = info.joined ? '● ' : '';
          const unreadSpan = info.unread > 0 ? \`<span class="ch-unread">\${info.unread}</span>\` : '';
          barHtml += \`<button class="ch-pill \${isActive ? 'active' : info.joined ? 'joined' : ''}" onclick="event.stopPropagation(); enterWorldChannel('\${chId}')" title="点击进入 #\${chId} (\${info.topic || ''})">\${dot}#\${chId}\${unreadSpan}</button>\`;
        }
      }
      barHtml += \`<button class="ch-pill \${state.showChannelPicker ? 'active' : ''}" onclick="event.stopPropagation(); runCmd('channels')" title="打开全部频道列表并选择进入">⊕ 频道目录</button>\`;
      document.getElementById('channel-bar').innerHTML = barHtml;

      // Render interactive Channel Directory when open
      const dirEl = document.getElementById('channel-directory');
      if (state.showChannelPicker) {
        dirEl.style.display = 'block';
        let dirHtml = \`<div class="channel-dir-header"><span>世界聊天频道目录 (点击任意频道立即加入并进入)</span><button class="ch-act-btn" onclick="event.stopPropagation(); state.showChannelPicker=false; render();">关闭 ×</button></div>\`;
        state.availableChannels.forEach((chId) => {
          const info = state.channels[chId] || { joined: false, topic: 'World channel #' + chId };
          const isCurrent = state.chatScope === 'world' && state.worldChannel === chId;
          dirHtml += \`<div class="channel-dir-row \${isCurrent ? 'selected' : ''}" onclick="event.stopPropagation(); enterWorldChannel('\${chId}')">\`;
          dirHtml += \`<div><strong style="color:\${info.joined ? 'var(--chat)' : 'var(--text)'}">\${info.joined ? '●' : '○'} #\${chId}</strong> <span style="color:var(--muted);font-size:11px;">\${info.topic || ''}</span></div>\`;
          dirHtml += \`<div class="channel-dir-actions">\`;
          dirHtml += \`<button class="ch-act-btn enter" onclick="event.stopPropagation(); enterWorldChannel('\${chId}')">\${isCurrent ? '当前所在' : '进入频道'}</button>\`;
          if (info.joined) {
            dirHtml += \`<button class="ch-act-btn leave" onclick="leaveWorldChannel('\${chId}', event)">退出</button>\`;
          }
          dirHtml += \`</div></div>\`;
        });
        dirEl.innerHTML = dirHtml;
      } else {
        dirEl.style.display = 'none';
      }

      let chatHtml = '';
      if (chObj.topic) {
        chatHtml += \`<div style="color:var(--muted)">topic: \${chObj.topic} (\${state.chatScope === 'game' ? '对局聊天' : '世界频道 #' + state.worldChannel})</div>\`;
      }
      for (const m of chObj.messages) {
        if (m.kind === 'sys') {
          chatHtml += \`<div class="sys-msg">· \${m.text}</div>\`;
        } else {
          chatHtml += \`<div><strong style="color:var(--chat)">\${m.author}:</strong> \${m.text}</div>\`;
        }
      }
      const chatBody = document.getElementById('chat-body');
      chatBody.innerHTML = chatHtml;
      chatBody.scrollTop = chatBody.scrollHeight;

      // Status & prompt
      const lastLog = state.logs[state.logs.length - 1];
      const logColor = lastLog?.level === 'err' ? 'var(--error)' : lastLog?.level === 'warn' ? 'var(--warning)' : 'var(--text)';
      const scopeTag = state.chatScope === 'game' ? \`game:\${state.gameId}\` : \`#\${state.worldChannel}\`;
      document.getElementById('status-line').innerHTML =
        \`<span style="color:var(--chat)">● authenticated · \${state.black}</span>  |  board: \${state.size}×\${state.size}  |  turn: \${toPlay}  |  channel: <span style="color:var(--chat)">\${scopeTag}</span>  |  <span style="color:\${logColor}">\${lastLog ? lastLog.text : ''}</span>\`;
      document.getElementById('prompt-label').textContent =
        state.estimateActive
          ? \`ogs[\${state.gameId}|estimate]>\`
          : state.chatScope === 'world'
          ? \`ogs[#\${state.worldChannel}]>\`
          : \`ogs[\${state.gameId}]>\`;
    }

    function updateFocusClasses() {
      document.getElementById('pane-board').classList.toggle('focused', state.focus === 'board');
      document.getElementById('pane-moves').classList.toggle('focused', state.focus === 'moves');
      document.getElementById('pane-chat').classList.toggle('focused', state.focus === 'chat');
    }

    function setFocus(pane) {
      state.focus = pane;
      updateFocusClasses();
      render();
    }

    function switchView(view) {
      const isTui = view === 'tui';
      document.getElementById('tui-view').style.display = isTui ? 'grid' : 'none';
      document.getElementById('source-view').style.display = isTui ? 'none' : 'grid';
      document.getElementById('tab-tui').classList.toggle('active', isTui);
      document.getElementById('tab-src').classList.toggle('active', !isTui);
      if (!isTui) loadFileList();
    }

    async function loadFileList() {
      const res = await fetch('/api/files').then(r => r.json());
      const container = document.getElementById('file-list');
      container.innerHTML = '';
      res.files.forEach((f, idx) => {
        const div = document.createElement('div');
        div.className = 'file-item' + (idx === 0 ? ' active' : '');
        div.textContent = f;
        div.onclick = () => {
          document.querySelectorAll('.file-item').forEach(el => el.classList.remove('active'));
          div.classList.add('active');
          loadFileContent(f);
        };
        container.appendChild(div);
      });
      if (res.files[0]) loadFileContent(res.files[0]);
    }

    async function loadFileContent(filePath) {
      const res = await fetch('/api/file?path=' + encodeURIComponent(filePath)).then(r => r.json());
      document.getElementById('code-viewer').textContent = res.content || res.error || '';
    }

    document.getElementById('cmd-input').addEventListener('keydown', (e) => {
      if (e.key === 'Enter') {
        runCmd(e.target.value);
      } else if (e.key === 'Tab') {
        e.preventDefault();
        const order = ['board', 'moves', 'chat'];
        const next = order[(order.indexOf(state.focus) + 1) % order.length];
        setFocus(next);
      }
    });

    render();
  </script>
</body>
</html>`;

const server = http.createServer((req, res) => {
  const url = new URL(req.url || '/', `http://${req.headers.host || 'localhost'}`);

  if (url.pathname === '/api/files' && req.method === 'GET') {
    const existing = SOURCE_FILES.filter((f) => fs.existsSync(path.join(__dirname, f)));
    res.writeHead(200, { 'Content-Type': 'application/json; charset=utf-8' });
    res.end(JSON.stringify({ files: existing }));
    return;
  }

  if (url.pathname === '/api/file' && req.method === 'GET') {
    const rel = url.searchParams.get('path') || '';
    const full = safeResolve(rel);
    if (!full || !fs.existsSync(full) || !fs.statSync(full).isFile()) {
      res.writeHead(404, { 'Content-Type': 'application/json; charset=utf-8' });
      res.end(JSON.stringify({ error: 'File not found' }));
      return;
    }
    const content = fs.readFileSync(full, 'utf-8');
    res.writeHead(200, { 'Content-Type': 'application/json; charset=utf-8' });
    res.end(JSON.stringify({ path: rel, content }));
    return;
  }

  if (url.pathname === '/api/sgf/save' && req.method === 'POST') {
    let body = '';
    req.on('data', (chunk) => { body += chunk; });
    req.on('end', () => {
      try {
        const { path: targetPath, content } = JSON.parse(body || '{}');
        const outPath = targetPath && targetPath.startsWith('/tmp/')
          ? targetPath
          : path.join('/tmp', path.basename(targetPath || 'game.sgf'));
        fs.writeFileSync(outPath, String(content || ''), 'utf-8');
        res.writeHead(200, { 'Content-Type': 'application/json; charset=utf-8' });
        res.end(JSON.stringify({ ok: true, path: outPath }));
      } catch (err) {
        res.writeHead(400, { 'Content-Type': 'application/json; charset=utf-8' });
        res.end(JSON.stringify({ ok: false, error: String(err.message || err) }));
      }
    });
    return;
  }

  if (url.pathname === '/api/sgf/load' && req.method === 'GET') {
    const targetPath = url.searchParams.get('path') || '/tmp/demo_game.sgf';
    const resolved = targetPath.startsWith('/tmp/')
      ? targetPath
      : safeResolve(targetPath);
    if (!resolved || !fs.existsSync(resolved)) {
      res.writeHead(404, { 'Content-Type': 'application/json; charset=utf-8' });
      res.end(JSON.stringify({ ok: false, error: `SGF file not found: ${targetPath}` }));
      return;
    }
    const content = fs.readFileSync(resolved, 'utf-8');
    res.writeHead(200, { 'Content-Type': 'application/json; charset=utf-8' });
    res.end(JSON.stringify({ ok: true, content }));
    return;
  }

  res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
  res.end(HTML_PAGE);
});

server.listen(PORT, '0.0.0.0', () => {
  console.log(`ogs-tui preview server listening on http://0.0.0.0:${PORT}`);
});
