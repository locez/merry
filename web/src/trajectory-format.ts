/** Escapes text before it is inserted into the inspector DOM. */
function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

/** Renders text or JSON with line numbers and token classes. */
export function renderCodeContent(content: string, kind: "json" | "text"): string {
  if (kind === "json") {
    return highlightJson(formatJsonContent(content));
  }
  const normalized = content.replace(/\r\n?/g, "\n");
  return normalized
    .split("\n")
    .map((line, index) => `
      <span class="code-line"><span class="code-line-number">${index + 1}</span><span class="code-line-content">${escapeHtml(line) || " "}</span></span>`)
    .join("");
}

/** Formats JSON without parsing numbers, preserving exact numeric lexemes. */
export function formatJsonContent(content: string): string {
  const output: string[] = [];
  const stack: string[] = [];
  let indent = 0;
  let inString = false;
  let escaped = false;
  let lastSignificant = "";

  const appendIndent = (): void => {
    output.push("  ".repeat(indent));
  };
  const nextSignificant = (start: number): string => {
    for (let index = start; index < content.length; index += 1) {
      if (!/\s/.test(content[index] ?? "")) {
        return content[index] ?? "";
      }
    }
    return "";
  };

  for (let index = 0; index < content.length; index += 1) {
    const character = content[index] ?? "";
    if (inString) {
      output.push(character);
      if (escaped) {
        escaped = false;
      } else if (character === "\\") {
        escaped = true;
      } else if (character === '"') {
        inString = false;
        lastSignificant = '"';
      }
      continue;
    }
    if (/\s/.test(character)) {
      continue;
    }
    if (character === '"') {
      inString = true;
      output.push(character);
      lastSignificant = '"';
      continue;
    }
    if (character === "{" || character === "[") {
      output.push(character);
      stack.push(character);
      if (nextSignificant(index + 1) !== (character === "{" ? "}" : "]")) {
        indent += 1;
        output.push("\n");
        appendIndent();
      }
    } else if (character === "}" || character === "]") {
      const opener = stack.pop();
      const expected = character === "}" ? "{" : "[";
      if (opener !== expected) {
        return content;
      }
      if (lastSignificant !== expected) {
        indent = Math.max(indent - 1, 0);
        output.push("\n");
        appendIndent();
      }
      output.push(character);
    } else if (character === ",") {
      output.push(",\n");
      appendIndent();
    } else if (character === ":") {
      output.push(": ");
    } else {
      output.push(character);
    }
    lastSignificant = character;
  }
  if (inString || stack.length > 0) {
    return content;
  }
  return output.join("");
}

/** Adds conservative syntax classes without changing the source text. */
export function highlightJson(content: string): string {
  const tokenPattern = /"(?:\\.|[^"\\])*"(?=\s*:)|"(?:\\.|[^"\\])*"|-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?|true|false|null/g;
  let output = "";
  let lastIndex = 0;
  let match: RegExpExecArray | null = tokenPattern.exec(content);
  while (match !== null) {
    const token = match[0];
    output += escapeHtml(content.slice(lastIndex, match.index));
    const tokenClass = token.startsWith('"')
      ? content.slice(tokenPattern.lastIndex).match(/^\s*:/) === null ? "json-string" : "json-key"
      : token === "true" || token === "false" ? "json-boolean" : token === "null" ? "json-null" : "json-number";
    output += `<span class="${tokenClass}">${escapeHtml(token)}</span>`;
    lastIndex = tokenPattern.lastIndex;
    match = tokenPattern.exec(content);
  }
  return output + escapeHtml(content.slice(lastIndex));
}
