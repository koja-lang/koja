import {
  commands,
  languages,
  window,
  workspace,
  ExtensionContext,
  TextDocument,
  TextEdit,
  Range,
  Position,
  FormattingOptions,
  CancellationToken,
} from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from "vscode-languageclient/node";
import { execFileSync } from "child_process";
import { writeFileSync, unlinkSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

let client: LanguageClient | undefined;

function getExpoBinary(): string {
  const config = workspace.getConfiguration("expo");
  return config.get<string>("path", "") || "expo";
}

function createClient(): LanguageClient {
  const config = workspace.getConfiguration("expo.lsp");
  const configPath = config.get<string>("path", "");
  const command = configPath || "expo-lsp";

  const serverOptions: ServerOptions = {
    command,
    args: [],
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: "file", language: "expo" }],
  };

  return new LanguageClient(
    "expo-lsp",
    "Expo Language Server",
    serverOptions,
    clientOptions,
  );
}

function runExpoCommand(subcommand: string) {
  const editor = window.activeTextEditor;
  if (!editor) {
    window.showErrorMessage("No active file to run.");
    return;
  }

  const doc = editor.document;
  if (doc.languageId !== "expo") {
    window.showErrorMessage("Active file is not an Expo file.");
    return;
  }

  if (doc.isUntitled) {
    window.showErrorMessage("Save the file before running.");
    return;
  }

  doc.save().then(() => {
    const binary = getExpoBinary();
    const filePath = doc.uri.fsPath;
    const terminal =
      window.terminals.find((t) => t.name === "Expo") ||
      window.createTerminal("Expo");
    terminal.show();
    terminal.sendText(`${binary} ${subcommand} "${filePath}"`);
  });
}

export function activate(context: ExtensionContext) {
  client = createClient();
  client.start();

  context.subscriptions.push(
    commands.registerCommand("expo.restartServer", async () => {
      if (client) {
        try {
          await client.stop();
        } catch {
          // Client may be in startFailed state, safe to ignore
        }
        client.dispose();
      }
      client = createClient();
      await client.start();
    }),

    commands.registerCommand("expo.runFile", () => {
      runExpoCommand("run");
    }),

    commands.registerCommand("expo.buildFile", () => {
      runExpoCommand("build");
    }),

    languages.registerDocumentFormattingEditProvider("expo", {
      provideDocumentFormattingEdits(
        document: TextDocument,
        _options: FormattingOptions,
        _token: CancellationToken,
      ): TextEdit[] {
        const binary = getExpoBinary();
        const text = document.getText();
        const tmpFile = join(tmpdir(), `expo-fmt-${Date.now()}.expo`);

        try {
          writeFileSync(tmpFile, text, "utf-8");
          const formatted = execFileSync(binary, ["format", tmpFile], {
            encoding: "utf-8",
            timeout: 10_000,
          });

          if (formatted === text) {
            return [];
          }

          const lastLine = document.lineCount - 1;
          const fullRange = new Range(
            new Position(0, 0),
            new Position(lastLine, document.lineAt(lastLine).text.length),
          );
          return [TextEdit.replace(fullRange, formatted)];
        } catch {
          return [];
        } finally {
          try {
            unlinkSync(tmpFile);
          } catch {
            // cleanup is best-effort
          }
        }
      },
    }),
  );
}

export function deactivate(): Thenable<void> | undefined {
  if (!client) {
    return undefined;
  }
  return client.stop();
}
