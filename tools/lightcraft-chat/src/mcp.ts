// Minimal MCP client over stdio: newline-delimited JSON-RPC 2.0 to a child process.
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";

export interface McpTool {
  name: string;
  description?: string;
  inputSchema: { type: "object"; [key: string]: unknown };
}

export interface McpContent {
  type: string;
  text?: string;
  data?: string;
  mimeType?: string;
}

export interface McpToolResult {
  content: McpContent[];
  isError?: boolean;
}

interface Pending {
  resolve: (value: unknown) => void;
  reject: (reason: Error) => void;
  timer: NodeJS.Timeout;
}

const REQUEST_TIMEOUT_MS = 300_000;

export class McpClient {
  private nextId = 1;
  private pending = new Map<number, Pending>();
  private buffer = "";
  private closed: Error | null = null;
  /** The server's own usage notes from `initialize`. */
  instructions = "";

  private constructor(private child: ChildProcessWithoutNullStreams) {
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk: string) => this.onData(chunk));
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk: string) => process.stderr.write(`[lightcraft] ${chunk}`));
    child.on("error", (err) => this.fail(new Error(`could not run the MCP server: ${err.message}`)));
    child.on("exit", (code, signal) =>
      this.fail(new Error(`the MCP server exited (${signal ?? `code ${code}`})`)),
    );
    // A write to a dead child must not take the whole server down.
    child.stdin.on("error", () => {});
  }

  static async start(command: string, args: string[]): Promise<McpClient> {
    const client = new McpClient(spawn(command, args, { stdio: ["pipe", "pipe", "pipe"] }));
    const init = (await client.request("initialize", {
      protocolVersion: "2025-06-18",
      capabilities: {},
      clientInfo: { name: "lightcraft-chat", version: "0.1.0" },
    })) as { instructions?: string };
    client.instructions = init.instructions ?? "";
    client.notify("notifications/initialized");
    return client;
  }

  async listTools(): Promise<McpTool[]> {
    const tools: McpTool[] = [];
    let cursor: string | undefined;
    do {
      const page = (await this.request("tools/list", cursor ? { cursor } : {})) as {
        tools: McpTool[];
        nextCursor?: string;
      };
      tools.push(...page.tools);
      cursor = page.nextCursor;
    } while (cursor);
    return tools;
  }

  async callTool(name: string, args: unknown): Promise<McpToolResult> {
    const result = (await this.request("tools/call", { name, arguments: args })) as McpToolResult;
    return { content: result.content ?? [], isError: result.isError === true };
  }

  close(): void {
    this.fail(new Error("the MCP client was closed"));
    this.child.stdin.end();
    this.child.kill();
  }

  private request(method: string, params: unknown): Promise<unknown> {
    if (this.closed) return Promise.reject(this.closed);
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`${method} timed out after ${REQUEST_TIMEOUT_MS / 1000} s`));
      }, REQUEST_TIMEOUT_MS);
      this.pending.set(id, { resolve, reject, timer });
      this.child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
    });
  }

  private notify(method: string): void {
    this.child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method })}\n`);
  }

  private onData(chunk: string): void {
    this.buffer += chunk;
    let newline: number;
    while ((newline = this.buffer.indexOf("\n")) >= 0) {
      const line = this.buffer.slice(0, newline).trim();
      this.buffer = this.buffer.slice(newline + 1);
      if (line) this.onLine(line);
    }
  }

  private onLine(line: string): void {
    let message: { id?: number; result?: unknown; error?: { code: number; message: string } };
    try {
      message = JSON.parse(line);
    } catch {
      return; // not a protocol message
    }
    if (typeof message.id !== "number") return; // a notification from the server
    const pending = this.pending.get(message.id);
    if (!pending) return;
    this.pending.delete(message.id);
    clearTimeout(pending.timer);
    if (message.error) {
      pending.reject(new Error(`${message.error.message} (JSON-RPC ${message.error.code})`));
    } else {
      pending.resolve(message.result);
    }
  }

  private fail(error: Error): void {
    if (this.closed) return;
    this.closed = error;
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pending.clear();
  }
}
