import { describe, expect, it } from "vitest";
import { format } from "../../dist/index.js";

// NOTE: For now, Vue files are partially handled by Prettier

describe("Format js-in-vue with prettier-plugin-oxfmt", () => {
  it("should format .vue w/ sort-imports", async () => {
    const input = `
<script lang="ts">
import z from "z";
  import a from "a";
    import m from "m";

</script>
<script lang="ts" setup>
import z from "z";
  import a from "a";
    import m from "m";

</script>
<template> <div>{{a+m+z}}</div> </template>
`;
    const result = await format("a.vue", input, {
      vueIndentScriptAndStyle: true,
      experimentalSortImports: {},
    });

    expect(result.code).toMatchSnapshot();
    expect(result.errors).toStrictEqual([]);
  });

  it("should format .vue w/ sort-tailwindcss", async () => {
    const input = `
<script setup>
import { ref } from "vue";
import clsx from "clsx";

const count = ref(0);
const cls = clsx("p-4 flex");
</script>
<template>
  <div class="flex p-4">{{count}}</div>
  <div class="p-4 flex">{{count}}</div>
</template>
`;
    const result = await format("a.vue", input, {
      vueIndentScriptAndStyle: true,
      experimentalSortImports: {},
      experimentalTailwindcss: { functions: ["clsx"] },
    });

    expect(result.code).toMatchSnapshot();
    expect(result.errors).toStrictEqual([]);
  });

  it("should format script blocks in staged internal vue mode", async () => {
    const input = `
<script lang="ts" setup>
import z from "z";
  import a from "a";
const answer=1
</script>
<template>   <div>{{answer}}</div> </template>
`;
    const result = await format("a.vue", input, {
      experimentalVueInternal: true,
      vueIndentScriptAndStyle: true,
      experimentalSortImports: {},
    });

    expect(result.code).toMatchSnapshot();
    expect(result.errors).toStrictEqual([]);
  });

  it("should format template-only vue files in staged internal mode", async () => {
    const input = '<template>   <Comp :label="`${ foo }`">{{msg}}</Comp> </template>\n';
    const result = await format("a.vue", input, {
      experimentalVueInternal: true,
    });

    expect(result.code).toMatchSnapshot();
    expect(result.errors).toStrictEqual([]);
  });

  it("should fallback to external formatter for unsupported custom SFC blocks", async () => {
    const input = `
<template><div>{{count}}</div></template>
<i18n lang="yaml">
message: hello
</i18n>
`;
    const internal = await format("a.vue", input, {
      experimentalVueInternal: true,
    });
    const external = await format("a.vue", input, {});

    expect(internal.errors).toStrictEqual([]);
    expect(external.errors).toStrictEqual([]);
    expect(internal.code).toBe(external.code);
  });

  it("should fallback to external formatter for unsupported complex event bindings", async () => {
    const input = `
<script setup lang="ts">
let x = 1;
</script>
<template>
  <div @click="if (x === (1 as number)) { x += 1; }">{{x}}</div>
</template>
`;
    const internal = await format("a.vue", input, {
      experimentalVueInternal: true,
    });
    const external = await format("a.vue", input, {});

    expect(internal.errors).toStrictEqual([]);
    expect(external.errors).toStrictEqual([]);
    expect(internal.code).toBe(external.code);
  });

  it("should fallback to external formatter for style blocks", async () => {
    const input = `
<template><div class="x">{{count}}</div></template>
<style>
.x{ display:flex; }
</style>
`;
    const internal = await format("a.vue", input, {
      experimentalVueInternal: true,
    });
    const external = await format("a.vue", input, {});

    expect(internal.errors).toStrictEqual([]);
    expect(external.errors).toStrictEqual([]);
    expect(internal.code).toBe(external.code);
  });

  it("should fallback to external formatter for multiline directive template literals", async () => {
    const input = `
<template>
  <Comp
    #default="{ a = \`line
\${foo}\` }"
  >{{ a }}</Comp>
</template>
`;
    const internal = await format("a.vue", input, {
      experimentalVueInternal: true,
    });
    const external = await format("a.vue", input, {});

    expect(internal.errors).toStrictEqual([]);
    expect(external.errors).toStrictEqual([]);
    expect(internal.code).toBe(external.code);
  });

  it("should fallback to external formatter for unindented multiline template content", async () => {
    const input = `
<template>
<span>{{(a||          b)}} {{z&&(a&&b)}}</span>
</template>
`;
    const internal = await format("a.vue", input, {
      experimentalVueInternal: true,
    });
    const external = await format("a.vue", input, {});

    expect(internal.errors).toStrictEqual([]);
    expect(external.errors).toStrictEqual([]);
    expect(internal.code).toBe(external.code);
  });
});
