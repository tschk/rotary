/**
 * Example pi-compatible extension for Rotary.
 *
 * This file uses the same ExtensionAPI interface as pi extensions.
 * It works with both:
 *   - pi (loaded via jiti in-process)
 *   - Rotary (loaded via Bun + our shim)
 *
 * It registers:
 *   - A "word_count" tool that counts words in text
 *   - A "/count" command that uses the tool
 *   - Event handlers for agent lifecycle events
 */

interface ExtensionAPI {
  on(event: string, handler: (event: any, ctx: any) => Promise<any>): void;
  registerTool(tool: any): void;
  registerCommand(name: string, def: any): void;
  sendMessage(message: any, options?: any): void;
  setSessionName(name: string): void;
}

export default function (pi: ExtensionAPI) {
  let turnCount = 0;

  pi.on("session_start", async (_event, ctx) => {
    ctx.ui?.notify("Word count extension loaded", "info");
    pi.setSessionName("word-count-session");
  });

  pi.on("turn_start", async (_event, ctx) => {
    turnCount++;
    ctx.ui?.setStatus("word-count", `Turn ${turnCount}`);
  });

  pi.on("turn_end", async (_event, ctx) => {
    ctx.ui?.setStatus("word-count", `Turn ${turnCount} done`);
  });

  pi.on("tool_call", async (event, _ctx) => {
    if (event.toolName === "bash") {
      const command = event.input?.command as string;
      if (command && /\brm\s+(-rf?|--recursive)/i.test(command)) {
        return { block: true, reason: "Dangerous command blocked by word-count extension" };
      }
    }
    return undefined;
  });

  pi.registerTool({
    name: "word_count",
    label: "Word Count",
    description: "Count the number of words in a given text string.",
    parameters: {
      type: "object",
      properties: {
        text: {
          type: "string",
          description: "The text to count words in.",
        },
      },
      required: ["text"],
    },
    execute: async (_toolCallId: string, params: { text: string }) => {
      const text = params.text ?? "";
      const words = text.trim().split(/\s+/).filter(Boolean);
      return {
        content: [
          {
            type: "text",
            text: `Word count: ${words.length}\nCharacter count: ${text.length}`,
          },
        ],
      };
    },
  });

  pi.registerCommand("count", {
    description: "Count words in the provided text",
    handler: async (args: string, ctx) => {
      if (!args) {
        ctx.ui.notify("Usage: /count <text>", "warning");
        return;
      }
      const words = args.trim().split(/\s+/).filter(Boolean);
      ctx.ui.notify(`Word count: ${words.length}`, "info");
    },
  });
}
