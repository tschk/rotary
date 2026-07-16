/**
 * Rotary pi-compatibility shim.
 *
 * Provides an ExtensionAPI-compatible interface to TypeScript extensions
 * designed for pi (https://github.com/earendil-works/pi). Translates calls
 * to JSONL over stdio, communicating with the Zig host process.
 *
 * Protocol (Bun subprocess ↔ Zig host):
 *
 * Host → Shim (stdin), JSONL:
 *   {"type":"init"}                                          — start the extension
 *   {"type":"event","event":"agent_start","data":{...}}      — forward agent event
 *   {"type":"tool_call","id":"...","name":"...","arguments":{...}} — execute tool
 *   {"type":"command","name":"...","args":"..."}             — execute registered command
 *   {"type":"ui_response","id":"...","value":"..."|null,"confirmed":bool|null,"cancelled":bool|null}
 *
 * Shim → Host (stdout), JSONL:
 *   {"type":"ready"}                                         — shim loaded, extension registered
 *   {"type":"register_tool","name":"...","description":"...","parameters":{...}}
 *   {"type":"register_command","name":"...","description":"..."}
 *   {"type":"register_provider","name":"...","config":{...}}
 *   {"type":"tool_result","id":"...","content":"...","is_error":false}
 *   {"type":"event_result","event":"...","result":{...}|null}
 *   {"type":"ui_request","id":"...","method":"select"|"confirm"|"input"|"editor"|"notify"|"setStatus"|"setWidget"|"setTitle"|"setEditorText",...}
 *   {"type":"error","message":"...","event":"..."}
 */

// ---- Minimal type definitions matching pi's ExtensionAPI ----

interface ExtensionEvent {
  [key: string]: any;
}

interface ExtensionContext {
  ui: UIContext;
  hasUI: boolean;
  mode: "rpc";
}

interface UIContext {
  select(title: string, options: string[], timeout?: number): Promise<string | undefined>;
  confirm(title: string, message: string, timeout?: number): Promise<boolean>;
  input(title: string, placeholder?: string, timeout?: number): Promise<string | undefined>;
  editor(title: string, prefill?: string): Promise<string | undefined>;
  notify(message: string, notifyType?: "info" | "warning" | "error"): void;
  setStatus(key: string, text?: string): void;
  setWidget(key: string, lines?: string[], placement?: "aboveEditor" | "belowEditor"): void;
  setTitle(title: string): void;
  setEditorText(text: string): void;
}

interface ToolDefinition {
  name: string;
  label?: string;
  description: string;
  parameters?: any;
  execute: (toolCallId: string, params: any, signal: AbortSignal, onUpdate: (details: any) => void, ctx: ExtensionContext) => Promise<ToolResult>;
  executionMode?: "sequential" | "parallel";
}

interface ToolResult {
  content: Array<{ type: string; text?: string }>;
  details?: any;
}

interface CommandDefinition {
  description?: string;
  handler: (args: string, ctx: ExtensionContext) => Promise<void>;
}

interface ProviderConfig {
  [key: string]: any;
}

interface ExtensionAPI {
  on(event: string, handler: (event: ExtensionEvent, ctx: ExtensionContext) => Promise<any>): void;
  registerTool(tool: ToolDefinition): void;
  registerCommand(name: string, def: CommandDefinition): void;
  registerProvider(name: string, config: ProviderConfig): void;
  sendMessage(message: any, options?: any): void;
  sendUserMessage(content: string, options?: any): void;
  appendEntry(type: string, data: any): void;
  setSessionName(name: string): void;
  getSessionName(): string;
  exec(command: string, args?: string[], options?: any): Promise<{ stdout: string; stderr: string; exitCode: number }>;
  setActiveTools(toolNames: string[]): void;
  getActiveTools(): string[];
  setModel(model: string): void;
  getThinkingLevel(): string;
  setThinkingLevel(level: string): void;
}

// ---- Implementation ----

const eventHandlers: Map<string, Array<(event: any, ctx: ExtensionContext) => Promise<any>>> = new Map();
const eventBus: Map<string, Array<(data: any) => void>> = new Map();
const tools: Map<string, ToolDefinition> = new Map();
const commands: Map<string, CommandDefinition> = new Map();
const pendingUiRequests: Map<string, { resolve: (v: any) => void; reject: (e: any) => void }> = new Map();
let sessionName = "";
let activeTools: string[] = [];
let thinkingLevel = "medium";
let currentModel = "";

let uiCounter = 0;

function sendToHost(msg: Record<string, any>): void {
  const line = JSON.stringify(msg);
  process.stdout.write(line + "\n");
}

function makeUIContext(): UIContext {
  function dialogRequest(method: string, params: Record<string, any>, hasResponse: boolean): Promise<any> {
    const id = `ui-${++uiCounter}`;
    sendToHost({ type: "ui_request", id, method, ...params });
    if (!hasResponse) return Promise.resolve(undefined);
    return new Promise((resolve, reject) => {
      pendingUiRequests.set(id, { resolve, reject });
    });
  }

  return {
    select: (title, options, timeout) => dialogRequest("select", { title, options, timeout }, true),
    confirm: (title, message, timeout) => dialogRequest("confirm", { title, message, timeout }, true).then((v: any) => v === true),
    input: (title, placeholder, timeout) => dialogRequest("input", { title, placeholder, timeout }, true),
    editor: (title, prefill) => dialogRequest("editor", { title, prefill }, true),
    notify: (message, notifyType = "info") => dialogRequest("notify", { message, notifyType }, false),
    setStatus: (key, text) => dialogRequest("setStatus", { statusKey: key, statusText: text }, false),
    setWidget: (key, lines, placement = "aboveEditor") => dialogRequest("setWidget", { widgetKey: key, widgetLines: lines, widgetPlacement: placement }, false),
    setTitle: (title) => dialogRequest("setTitle", { title }, false),
    setEditorText: (text) => dialogRequest("setEditorText", { text }, false),
  };
}

function makeContext(): ExtensionContext {
  return {
    ui: makeUIContext(),
    hasUI: true,
    mode: "rpc",
  };
}

const api: ExtensionAPI = {
  on(event, handler) {
    if (!eventHandlers.has(event)) eventHandlers.set(event, []);
    eventHandlers.get(event)!.push(handler);
  },

  registerTool(tool) {
    tools.set(tool.name, tool);
    sendToHost({
      type: "register_tool",
      name: tool.name,
      description: tool.description,
      parameters: tool.parameters ?? {},
    });
  },

  registerCommand(name, def) {
    commands.set(name, def);
    sendToHost({
      type: "register_command",
      name,
      description: def.description ?? "",
    });
  },

  registerProvider(name, config) {
    sendToHost({ type: "register_provider", name, config });
  },

  sendMessage(message, options) {
    sendToHost({ type: "send_message", message, options });
  },

  sendUserMessage(content, options) {
    sendToHost({ type: "send_user_message", content, options });
  },

  appendEntry(type, data) {
    sendToHost({ type: "append_entry", entry_type: type, data });
  },

  setSessionName(name) {
    sessionName = name;
    sendToHost({ type: "set_session_name", name });
  },

  getSessionName() {
    return sessionName;
  },

  async exec(command, args = [], options = {}) {
    const proc = Bun.spawn([command, ...args], {
      stdout: "pipe",
      stderr: "pipe",
      ...options,
    });
    const [stdout, stderr] = await Promise.all([
      new Response(proc.stdout).text(),
      new Response(proc.stderr).text(),
    ]);
    const exitCode = await proc.exited;
    return { stdout, stderr, exitCode };
  },

  setActiveTools(names) {
    activeTools = names;
    sendToHost({ type: "set_active_tools", tools: names });
  },

  getActiveTools() {
    return activeTools;
  },

  setModel(model) {
    currentModel = model;
    sendToHost({ type: "set_model", model });
  },

  getThinkingLevel() {
    return thinkingLevel;
  },

  setThinkingLevel(level) {
    thinkingLevel = level;
    sendToHost({ type: "set_thinking_level", level });
  },

  events: {
    on(name: string, handler: (data: any) => void) {
      if (!eventBus.has(name)) eventBus.set(name, []);
      eventBus.get(name)!.push(handler);
    },
    emit(name: string, data?: any) {
      const handlers = eventBus.get(name);
      if (handlers) for (const h of handlers) h(data);
    },
    off(name: string, handler?: (data: any) => void) {
      if (!handler) { eventBus.delete(name); return; }
      const handlers = eventBus.get(name);
      if (handlers) {
        const idx = handlers.indexOf(handler);
        if (idx >= 0) handlers.splice(idx, 1);
      }
    },
  },

  getAllTools() {
    return Array.from(tools.values()).map((t: any) => ({
      name: t.name,
      label: t.label,
      description: t.description,
    }));
  },

  registerShortcut(_shortcut) {
    // Shortcuts are TUI-specific, degraded in RPC mode
  },

  registerFlag(_flag, _description, _defaultValue) {
    // Flags are TUI-specific, degraded in RPC mode
  },

  registerMessageRenderer(_matcher, _renderer) {
    // Message renderers are TUI-specific, degraded in RPC mode
  },

  registerEntryRenderer(_type, _renderer) {
    // Entry renderers are TUI-specific, degraded in RPC mode
  },
};

// ---- stdin reader ----

let inputBuffer = "";
let messageQueue: any[] = [];
let extensionReady = false;

process.stdin.setEncoding("utf-8");

process.stdin.on("data", (chunk: string) => {
  inputBuffer += chunk;
  let newlineIdx: number;
  while ((newlineIdx = inputBuffer.indexOf("\n")) !== -1) {
    let line = inputBuffer.slice(0, newlineIdx);
    inputBuffer = inputBuffer.slice(newlineIdx + 1);
    if (line.endsWith("\r")) line = line.slice(0, -1);
    if (line.length === 0) continue;
    try {
      const msg = JSON.parse(line);
      if (extensionReady) {
        handleHostMessage(msg);
      } else if (msg.type === "init") {
        handleHostMessage(msg);
      } else {
        messageQueue.push(msg);
      }
    } catch (err) {
      sendToHost({ type: "error", message: `Failed to handle message: ${err}` });
    }
  }
});

async function flushQueue(): Promise<void> {
  extensionReady = true;
  for (const msg of messageQueue) {
    await handleHostMessage(msg);
  }
  messageQueue = [];
}

async function handleHostMessage(msg: any): Promise<void> {
  switch (msg.type) {
    case "init": {
      // Load and run the extension
      let extPath = msg.path ?? process.argv[2];
      if (!extPath) {
        sendToHost({ type: "error", message: "No extension path provided" });
        return;
      }
      // Resolve relative to cwd
      if (!extPath.startsWith("/") && !extPath.startsWith("./") && !extPath.startsWith("../")) {
        extPath = "./" + extPath;
      }
      const absPath = require("path").resolve(extPath);
      try {
        const mod = await import(absPath);
        const extFn = mod.default ?? mod;
        if (typeof extFn === "function") {
          await extFn(api);
        }
        sendToHost({ type: "ready" });
        await flushQueue();
      } catch (err) {
        sendToHost({ type: "error", message: `Failed to load extension: ${err}` });
      }
      break;
    }

    case "event": {
      const handlers = eventHandlers.get(msg.event);
      if (!handlers) return;
      const ctx = makeContext();
      for (const handler of handlers) {
        try {
          const result = await handler(msg.data ?? {}, ctx);
          if (result !== undefined) {
            sendToHost({ type: "event_result", event: msg.event, result });
          }
        } catch (err) {
          sendToHost({ type: "error", message: `Event handler error: ${err}`, event: msg.event });
        }
      }
      break;
    }

    case "tool_call": {
      const tool = tools.get(msg.name);
      if (!tool) {
        sendToHost({
          type: "tool_result",
          id: msg.id,
          content: JSON.stringify({ error: `Unknown tool: ${msg.name}` }),
          is_error: true,
        });
        return;
      }
      const ctx = makeContext();
      const controller = new AbortController();
      let args = msg.arguments;
      if (typeof args === "string") {
        try { args = JSON.parse(args); } catch { args = {}; }
      }
      if (!args || typeof args !== "object") args = {};
      try {
        const result = await tool.execute(
          msg.id,
          args,
          controller.signal,
          () => {},
          ctx,
        );
        const textParts = (result.content ?? [])
          .filter((c: any) => c.type === "text" && c.text)
          .map((c: any) => c.text)
          .join("\n");
        sendToHost({
          type: "tool_result",
          id: msg.id,
          content: textParts,
          is_error: false,
        });
      } catch (err) {
        sendToHost({
          type: "tool_result",
          id: msg.id,
          content: `Tool execution failed: ${err}`,
          is_error: true,
        });
      }
      break;
    }

    case "command": {
      const cmd = commands.get(msg.name);
      if (!cmd) {
        sendToHost({ type: "error", message: `Unknown command: ${msg.name}` });
        return;
      }
      const ctx = makeContext();
      try {
        await cmd.handler(msg.args ?? "", ctx);
      } catch (err) {
        sendToHost({ type: "error", message: `Command error: ${err}` });
      }
      break;
    }

    case "ui_response": {
      const pending = pendingUiRequests.get(msg.id);
      if (!pending) return;
      pendingUiRequests.delete(msg.id);
      if (msg.cancelled) {
        pending.resolve(undefined);
      } else if (msg.confirmed !== undefined && msg.confirmed !== null) {
        pending.resolve(msg.confirmed);
      } else {
        pending.resolve(msg.value);
      }
      break;
    }

    default: {
      sendToHost({ type: "error", message: `Unknown message type: ${msg.type}` });
    }
  }
}

// Signal ready to receive init
sendToHost({ type: "shim_loaded" });
