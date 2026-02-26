import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join, relative } from "node:path";
import prettier from "prettier";
import { describe, expect, it } from "vitest";
import { format } from "../../dist/index.js";

// NOTE: Fixtures can be downloaded by `pnpm download-prettier-fixtures`
const FIXTURES_DIR = join(import.meta.dirname, "../../prettier-fixtures");
const MAX_INTERNAL_FIXTURES = 25;
const USE_PRETTIER_FIXTURES = process.env.OXFMT_VUE_CONFORMANCE_USE_PRETTIER_FIXTURES === "1";

describe("experimentalVueInternal differential report", () => {
  const prettierFixtures = USE_PRETTIER_FIXTURES
    ? collectFixtures(".vue", [
        "vue/range/example.vue",
        "vue/multiparser/lang-tsx.vue",
      ])
    : [];
  const vueFixtures = prettierFixtures
    .filter(({ name }) => name.startsWith("vue/"))
    .slice(0, MAX_INTERNAL_FIXTURES);

  if (USE_PRETTIER_FIXTURES && prettierFixtures.length === 0) {
    // eslint-disable-next-line no-console
    console.warn(
      "[vue-internal-conformance] OXFMT_VUE_CONFORMANCE_USE_PRETTIER_FIXTURES=1 but no prettier-fixtures found; falling back to edge-only suite",
    );
  }

  const activeFixtures = vueFixtures;
  activeFixtures.push(
    {
      name: "edge/template-interpolation-spacing.vue",
      content: `<template>   <div>{{answer}}</div> </template>\n`,
    },
    {
      name: "edge/template-interpolation-multiline.vue",
      content: `<template>\n  <Comp :label="\`\${ foo }\`">{{msg}}</Comp>\n</template>\n`,
    },
    {
      name: "edge/script-and-template.vue",
      content: `<script setup lang="ts">\nimport z from "z"\nimport a from "a"\nconst answer=1\n</script>\n<template>   <div>{{answer}}</div> </template>\n`,
    },
    {
      name: "edge/directive-expressions.vue",
      content:
        '<template><button @click="count+=1" v-if="foo&&bar">{{foo+bar}}</button></template>\n',
    },
    {
      name: "edge/v-for-expression.vue",
      content: '<template><li v-for="(item,index) in items">{{item+index}}</li></template>\n',
    },
    {
      name: "edge/v-slot-bindings.vue",
      content: '<template><Comp v-slot="{foo=1,bar}">{{foo+bar}}</Comp></template>\n',
    },
    {
      name: "edge/vue-bindings-multiline-template-literal.vue",
      content: `<template>
  <Comp
    #default="{ a = \`line
\${foo}\` }"
  >{{ a }}</Comp>
</template>
`,
    },
    {
      name: "edge/vue-for-multiline-template-literal.vue",
      content: `<template>
  <div v-for="(item = \`line
\${foo}\`, index) in items">{{ item }} - {{ index }}</div>
</template>
`,
    },
  );

  describe("report summary", () => {
    it.each([
      { printWidth: 80 },
      {
        printWidth: 100,
        vueIndentScriptAndStyle: true,
        singleQuote: true,
      },
    ])("produces differential summary %j", async (options) => {
      const records = await Promise.all(
        activeFixtures.map(async ({ name, content }) => {
          const [internalResult, prettierResult] = await compareWithPrettierUsingInternalMode(
            name,
            content,
            options,
          );
          return {
            name,
            internalResult,
            prettierResult,
            equalsPrettier: internalResult === prettierResult,
            changedFromInput: internalResult !== content,
            isError: internalResult === "ERROR",
            templateParity:
              internalResult !== "ERROR" &&
              prettierResult !== "ERROR" &&
              compareTagContents(internalResult, prettierResult, "template"),
            scriptParity:
              internalResult !== "ERROR" &&
              prettierResult !== "ERROR" &&
              compareTagContents(internalResult, prettierResult, "script"),
          };
        }),
      );

      const mismatches = records.filter((record) => !record.equalsPrettier).map((record) => record.name);
      const errors = records.filter((record) => record.isError).map((record) => record.name);
      const changedFromInput = records.filter((record) => record.changedFromInput).length;
      const mismatchKinds = {
        templateOnly: 0,
        scriptOnly: 0,
        templateAndScript: 0,
        unresolved: 0,
      };
      for (const record of records) {
        if (record.equalsPrettier || record.isError) continue;

        if (record.templateParity === false && record.scriptParity === true) {
          mismatchKinds.templateOnly += 1;
        } else if (record.templateParity === true && record.scriptParity === false) {
          mismatchKinds.scriptOnly += 1;
        } else if (record.templateParity === false && record.scriptParity === false) {
          mismatchKinds.templateAndScript += 1;
        } else {
          mismatchKinds.unresolved += 1;
        }
      }

      const summary = {
        fixtureSource: prettierFixtures.length > 0 ? "edge+prettier" : "edge-only",
        fixtures: records.length,
        errors: errors.length,
        equalToPrettier: records.length - mismatches.length,
        mismatches: mismatches.length,
        changedFromInput,
        mismatchKinds,
        mismatchSample: mismatches.slice(0, 10),
        errorSample: errors.slice(0, 10),
        note:
          prettierFixtures.length > 0
            ? null
            : "Set OXFMT_VUE_CONFORMANCE_USE_PRETTIER_FIXTURES=1 and run `pnpm download-prettier-fixtures` in apps/oxfmt for broader differential coverage",
      };

      expect(summary.errors).toBe(0);
      expect(summary).toMatchSnapshot();
    });
  });
});

// ---

type TestCase = { name: string; content: string };

function collectFixtures(ext: string, excludes: string[] = []): TestCase[] {
  const dir = FIXTURES_DIR;
  // NOTE: In CI, the fixtures might not be present, just skip and only run edge cases.
  if (!existsSync(dir)) return [];

  const results: TestCase[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true, recursive: true })) {
    if (!entry.isFile() || !entry.name.endsWith(ext)) continue;

    const fullPath = join(entry.parentPath, entry.name);
    const relPath = relative(dir, fullPath);
    if (excludes.some((value) => relPath.includes(value))) continue;

    results.push({ name: relPath, content: readFileSync(fullPath, "utf8") });
  }

  return results.sort((a, b) => a.name.localeCompare(b.name));
}

async function compareWithPrettierUsingInternalMode(
  fileName: string,
  content: string,
  options = {},
) {
  let prettierResult;
  try {
    prettierResult = await prettier.format(content, {
      parser: "vue",
      filepath: fileName,
      ...options,
    });
  } catch {
    prettierResult = "ERROR";
  }

  let internalResult;
  const internalResponse = await format(fileName, content, {
    ...options,
    experimentalVueInternal: true,
  });
  if (internalResponse.errors.length !== 0) {
    internalResult = "ERROR";
  } else {
    internalResult = internalResponse.code;
  }

  return [internalResult, prettierResult];
}

function compareTagContents(left: string, right: string, tagName: string) {
  const leftBlocks = extractTagContents(left, tagName);
  const rightBlocks = extractTagContents(right, tagName);
  return JSON.stringify(leftBlocks) === JSON.stringify(rightBlocks);
}

function extractTagContents(source: string, tagName: string): string[] {
  const openPrefix = `<${tagName}`;
  const closeTag = `</${tagName}>`;
  const contents: string[] = [];
  let cursor = 0;

  while (cursor < source.length) {
    const openStart = source.indexOf(openPrefix, cursor);
    if (openStart === -1) break;

    const openEnd = source.indexOf(">", openStart + openPrefix.length);
    if (openEnd === -1) break;

    const contentStart = openEnd + 1;
    const closeStart = source.indexOf(closeTag, contentStart);
    if (closeStart === -1) break;

    contents.push(source.slice(contentStart, closeStart));
    cursor = closeStart + closeTag.length;
  }

  return contents;
}
