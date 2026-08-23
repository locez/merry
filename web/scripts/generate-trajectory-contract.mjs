import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(scriptDirectory, "../..");
const schemaPath = resolve(repositoryRoot, "crates/merry-core/schema/trajectory-event.json");
const typesPath = resolve(repositoryRoot, "web/src/trajectory-contract.generated.ts");
const checkOnly = process.argv.includes("--check");

const schema = JSON.parse(readFileSync(schemaPath, "utf8"));
const typesText = `${renderTypescript(schema)}\n`;

writeOrCheck(typesPath, typesText);

function writeOrCheck(path, content) {
  if (checkOnly) {
    const current = readFileSync(path, "utf8");
    if (current !== content) {
      throw new Error(`${path} is stale; run npm run generate:trajectory-contract`);
    }
    return;
  }
  writeFileSync(path, content);
}

function renderTypescript(rootSchema) {
  const context = { definitions: rootSchema.$defs ?? {} };
  const lines = [
    "// This file is generated from merry-core's TrajectoryEvent JSON Schema.",
    "// Do not edit it directly; run npm run generate:trajectory-contract.",
    "",
    "export type JsonValue = null | boolean | number | string | JsonValue[] | JsonObject;",
    "export type JsonObject = { readonly [key: string]: JsonValue };",
    "export type JsonSchemaValue = boolean | JsonObject;",
    "export type Schema = JsonSchemaValue;",
    "export type WireInteger = bigint;",
    "",
  ];

  for (const name of Object.keys(context.definitions).sort()) {
    if (name === "Schema") {
      continue;
    }
    lines.push(`export type ${name} = ${renderType(context.definitions[name], context)};`);
    const values = stringConstants(context.definitions[name]);
    if (values.length > 0) {
      lines.push(`export const ${constantName(name, "VALUES")} = ${JSON.stringify(values)} as const;`);
    }
    appendFieldConstants(lines, name, context.definitions[name]);
    lines.push("");
  }

  appendFieldConstants(lines, "TrajectoryEvent", rootSchema);
  lines.push("");
  lines.push(`export type TrajectoryEvent = ${renderType(rootSchema, context)};`);
  lines.push("export type ArtifactReference = ArtifactRef;");
  lines.push("export type Diagnostic = ErrorInfo;");
  return lines.join("\n");
}

function renderType(schema, context) {
  if (!isObject(schema)) {
    throw new Error("trajectory schema contains an invalid schema node");
  }
  if (schema.$ref !== undefined) {
    return referenceName(schema.$ref);
  }
  if (schema["x-merry-wire-type"] === "u64") {
    return hasNull(schema) ? "WireInteger | null" : "WireInteger";
  }
  if (Object.hasOwn(schema, "const")) {
    return JSON.stringify(schema.const);
  }
  if (Array.isArray(schema.anyOf)) {
    return renderUnion(schema.anyOf.map((item) => renderType(item, context)));
  }
  if (Array.isArray(schema.oneOf)) {
    return renderUnion(schema.oneOf.map((item) => renderType(item, context)));
  }
  if (Array.isArray(schema.type)) {
    if (schema.type.includes("object") && schema.type.includes("boolean")) {
      return "JsonSchemaValue";
    }
    return renderUnion(schema.type.map((type) => renderType({ type }, context)));
  }
  if (schema.type === "object") {
    return renderObject(schema, context);
  }
  if (schema.type === "array") {
    return `readonly ${renderType(schema.items, context)}[]`;
  }
  if (schema.type === "string") {
    return "string";
  }
  if (schema.type === "boolean") {
    return "boolean";
  }
  if (schema.type === "null") {
    return "null";
  }
  if (schema.type === "integer" || schema.type === "number") {
    return "number";
  }
  throw new Error(`unsupported trajectory schema node: ${JSON.stringify(schema)}`);
}

function renderObject(schema, context) {
  const properties = schema.properties ?? {};
  if (Object.keys(properties).length === 0 && schema.additionalProperties === true) {
    return "JsonObject";
  }
  if (Object.keys(properties).length === 0) {
    return "Record<string, never>";
  }

  const required = new Set(schema.required ?? []);
  const fields = Object.keys(properties).sort().map((name) => {
    const property = properties[name];
    const isRequired = required.has(name)
      || property["x-merry-output-required"] === true
      || Object.hasOwn(property, "default");
    const optional = isRequired ? "" : "?";
    return `  readonly ${JSON.stringify(name)}${optional}: ${renderType(property, context)};`;
  });
  return `{\n${fields.join("\n")}\n}`;
}

function renderUnion(types) {
  return [...new Set(types)].join(" | ");
}

function stringConstants(schema) {
  const variants = schema.oneOf ?? [];
  if (!Array.isArray(variants) || variants.length === 0) {
    return schema.type === "string" && typeof schema.const === "string" ? [schema.const] : [];
  }
  const values = variants.map((variant) => variant.const);
  return values.every((value) => typeof value === "string") ? values : [];
}

function appendFieldConstants(lines, name, schema) {
  for (const variant of objectVariants(schema)) {
    const suffix = variant.variant === null ? "FIELDS" : `${variant.variant.toUpperCase()}_FIELDS`;
    lines.push(`export const ${constantName(name, suffix)} = ${JSON.stringify(variant.fields)} as const;`);
  }
}

function objectVariants(schema) {
  if (schema.type === "object" && isObject(schema.properties)) {
    return [{ variant: null, fields: Object.keys(schema.properties).sort() }];
  }
  if (!Array.isArray(schema.oneOf)) {
    return [];
  }
  return schema.oneOf.flatMap((variant) => {
    const discriminator = variant.properties?.type?.const;
    if (typeof discriminator !== "string" || !isObject(variant.properties)) {
      return [];
    }
    return [{ variant: discriminator, fields: Object.keys(variant.properties).sort() }];
  });
}

function constantName(name, suffix) {
  return `${name.replaceAll(/([a-z0-9])([A-Z])/g, "$1_$2").toUpperCase()}_${suffix}`;
}

function referenceName(reference) {
  const name = reference.split("/").at(-1);
  if (name === undefined) {
    throw new Error(`invalid trajectory schema reference: ${reference}`);
  }
  if (name === "ToolCallArguments") {
    return "JsonObject";
  }
  if (name === "Schema") {
    return "JsonSchemaValue";
  }
  return name;
}

function hasNull(schema) {
  if (Array.isArray(schema.type) && schema.type.includes("null")) {
    return true;
  }
  return Array.isArray(schema.anyOf)
    ? schema.anyOf.some((item) => item.type === "null")
    : false;
}

function isObject(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
