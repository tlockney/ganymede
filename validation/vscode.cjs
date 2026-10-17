// Replays the VS Code Jupyter extension's remote-server flow
// (vscode-jupyter e116e22: userServerUrlProvider.ts, jupyterPasswordConnect.ts,
// jupyterUtils.ts, jupyterRequestCreator.node.ts, jupyterLabHelper.ts,
// jupyterKernelSessionFactory.ts) against a server URL with ?token=.
// Usage: node vscode.cjs 'http://127.0.0.1:8888/?token=<token>' <kernel name>
// Env: CODE (default 6*7), STDIN_CODE (a cell that reads one line of input),
//      SLEEP_CODE (a long cell to interrupt). Without them those steps are skipped.
// Exits non-zero if any step fails.
const fetch = require('node-fetch');
const WebSocketWs = require('ws');
const { KernelAPI, ServerConnection, KernelSpecManager, KernelManager, SessionManager, ContentsManager } = require('@jupyterlab/services');
const crypto = require('crypto');

const [rawUrl, kernelName] = process.argv.slice(2);
const log = [];
const step = (name, ok, detail = '') => { log.push({ name, ok, detail }); console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ' - ' + detail : ''}`); };

(async () => {
  // A. parse
  const url = new URL(rawUrl.replace(/\/(lab|tree)(\??)$/, ''));
  const token = url.searchParams.get('token') || '';
  const baseUrl = `${url.protocol}//${url.host}${['/lab', '/tree'].includes(url.pathname) ? '/' : url.pathname}`;

  // B3. needPassword
  const r1 = await fetch(baseUrl, { redirect: 'follow', headers: { Connection: 'keep-alive' } });
  let requiresPassword;
  if (r1.status === 200 && r1.redirected && r1.url.includes('/login?')) requiresPassword = true;
  else if (r1.status === 200 && r1.redirected && r1.url.includes('/lab?')) requiresPassword = false;
  else {
    const r2 = await fetch(baseUrl + 'tree?', { redirect: 'manual', headers: { Connection: 'keep-alive' } });
    requiresPassword = r2.status === 404 ? false : r2.status !== 200;
    log.push({ tree: r2.status });
  }
  step('password probe (GET /, then /tree?)', true, `/ -> ${r1.status} at ${r1.url}; requiresPassword=${requiresPassword}`);
  // With a token present, requiresPassword is harmless (no login flow).

  // C. settings, with the extension's Request and WebSocket overrides.
  const authHeader = { Authorization: `token ${token}` };
  class JupyterRequest extends fetch.Request {
    constructor(input, init) {
      const headers = new fetch.Headers((init && init.headers) || {});
      for (const [k, v] of Object.entries(authHeader)) headers.set(k, v);
      headers.set('Content-Type', 'application/json');
      super(input, { ...init, headers });
      // Like the extension, refuse the library's own Authorization append.
      const append = this.headers.append.bind(this.headers);
      this.headers.append = (k, v) => { if (k.toLowerCase() !== 'authorization') append(k, v); };
    }
  }
  class JupyterWebSocket extends WebSocketWs {
    constructor(u, protocols) {
      super(u, protocols, { headers: authHeader });
      if (!/.*\/kernels\/(.*)\/.*/.test(u) && !u.includes('/api/events')) console.error('kernel id not parsed from', u);
      this.pinger = setInterval(() => this.readyState === 1 && this.ping(), 30000);
      this.on('close', () => clearInterval(this.pinger));
    }
  }
  const serverSettings = ServerConnection.makeSettings({
    baseUrl, appUrl: '', wsUrl: baseUrl.replace('http', 'ws'), token, appendToken: true,
    init: { cache: 'no-store', credentials: 'same-origin' },
    fetch, Request: JupyterRequest, Headers: fetch.Headers, WebSocket: JupyterWebSocket,
  });
  const kernelManager = new KernelManager({ serverSettings });
  const sessionManager = new SessionManager({ serverSettings, kernelManager });
  const specsManager = new KernelSpecManager({ serverSettings });
  new ContentsManager({ serverSettings });

  // B6/D1/D2: validateRemoteUri
  const running = [...(await KernelAPI.listRunning(serverSettings))];
  step('GET api/kernels (listRunning)', true, `${running.length} running`);
  await sessionManager.ready;
  await specsManager.refreshSpecs();
  const specs = specsManager.specs;
  const names = Object.keys(specs.kernelspecs);
  step('GET api/kernelspecs', names.includes(kernelName), `default=${specs.default}, specs=${names.join(',')}`);
  const s = specs.kernelspecs[kernelName];
  step('kernelspec has fields the extension reads', !!(s && s.argv && s.display_name && s.language && s.resources), JSON.stringify(s && s.resources));

  // D3
  await sessionManager.refreshRunning();
  step('GET api/sessions', true, `${[...sessionManager.running()].length} sessions`);

  // D4: start a session with the extension's backing-file names
  const uuid = () => crypto.randomUUID().replace(/-/g, '');
  const nb = 'Untitled-1';
  const session = await sessionManager.startNew(
    { path: `${nb}-jvsc-${uuid()}${uuid()}.ipynb`, type: 'notebook', name: `${nb}-${uuid()}.ipynb`, kernel: { name: kernelName } },
    { kernelConnectionOptions: { handleComms: true } });
  step('POST api/sessions', !!session.kernel, `session ${session.id}, kernel ${session.kernel && session.kernel.id}`);

  // D5/D6: websocket, kernel_info, idle
  const kernel = session.kernel;
  await new Promise((res, rej) => { const t = setTimeout(() => rej(new Error('no idle')), 30000);
    const check = () => { if (kernel.status === 'idle' && kernel.connectionStatus === 'connected') { clearTimeout(t); res(); } };
    kernel.statusChanged.connect(check); kernel.connectionStatusChanged.connect(check); check(); });
  const info = await kernel.info;
  step('websocket connected, kernel idle, kernel_info', true, `subprotocol=${kernel._ws && kernel._ws.protocol}, impl=${info.implementation}, lang=${info.language_info.name}`);

  const run = async code => { const out = []; const f = kernel.requestExecute({ code, allow_stdin: true });
    f.onIOPub = m => { if (m.header.msg_type === 'stream') out.push(m.content.text); if (m.header.msg_type === 'execute_result') out.push(m.content.data['text/plain']); };
    f.onStdin = m => kernel.sendInputReply({ status: 'ok', value: 'typed' }, m.header);
    const reply = await f.done; return { status: reply.content.status, out: out.join('') }; };
  const e1 = await run(process.env.CODE || '6*7');
  step('execute', e1.status === 'ok', JSON.stringify(e1));
  if (process.env.STDIN_CODE) { const e = await run(process.env.STDIN_CODE); step('stdin round trip', e.out.includes('typed'), JSON.stringify(e)); }

  // D8 interrupt during a long cell
  if (process.env.SLEEP_CODE) {
    const f = kernel.requestExecute({ code: process.env.SLEEP_CODE });
    await new Promise(r => setTimeout(r, 1000));
    await kernel.interrupt();
    const reply = await f.done.catch(e => e);
    step('POST api/kernels/{id}/interrupt', reply.content && reply.content.status === 'error', reply.content && reply.content.ename);
  }

  // D9 restart
  await kernel.restart();
  await new Promise((res, rej) => { const t = setTimeout(() => rej(new Error('no idle after restart')), 30000);
    const check = () => { if (kernel.status === 'idle' && kernel.connectionStatus === 'connected') { clearTimeout(t); res(); } };
    kernel.statusChanged.connect(check); kernel.connectionStatusChanged.connect(check); check(); });
  const e2 = await run(process.env.CODE || '6*7');
  step('POST api/kernels/{id}/restart then execute', e2.status === 'ok', JSON.stringify(e2));

  // D7: a second client connects to the live kernel (reopening the notebook)
  const other = new SessionManager({ serverSettings, kernelManager: new KernelManager({ serverSettings }) });
  await other.ready; await other.refreshRunning();
  const model = [...other.running()].find(m => m.id === session.id);
  const s2 = other.connectTo({ model });
  await new Promise((res, rej) => { const t = setTimeout(() => rej(new Error('connectTo never connected')), 15000);
    const check = () => { if (s2.kernel.connectionStatus === 'connected') { clearTimeout(t); res(); } };
    s2.kernel.connectionStatusChanged.connect(check); check(); });
  step('connectTo existing session (second websocket)', true);
  s2.dispose();

  // D10 shutdown
  await session.shutdown();
  await sessionManager.refreshRunning();
  step('DELETE api/sessions/{id}', ![...sessionManager.running()].some(m => m.id === session.id));
  sessionManager.dispose(); kernelManager.dispose(); other.dispose();
  process.exit(log.every(l => l.ok) ? 0 : 1);
})().catch(e => { console.error('FAIL', e && (e.stack || e)); process.exit(1); });
