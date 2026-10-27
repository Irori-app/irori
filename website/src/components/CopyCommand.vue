<script setup lang="ts">
import { Check, Copy } from '@lucide/vue'
import { ref } from 'vue'

const props = defineProps<{ command: string }>()
const copied = ref(false)
const code = ref<HTMLElement>()

async function copy() {
  try {
    await navigator.clipboard.writeText(props.command)
    copied.value = true
    setTimeout(() => (copied.value = false), 1600)
  } catch {
    // Without clipboard access, select the command so the visitor can copy it.
    const range = document.createRange()
    if (code.value) range.selectNodeContents(code.value)
    const selection = window.getSelection()
    selection?.removeAllRanges()
    selection?.addRange(range)
  }
}
</script>

<template>
  <div class="flex min-w-0 items-start gap-2 rounded-lg border border-grain bg-ash py-2 pl-3 pr-2">
    <span aria-hidden="true" class="select-none pt-px font-mono text-sm text-tatami">$</span>
    <code ref="code" class="min-w-0 flex-1 overflow-x-auto whitespace-pre pt-px font-mono text-sm text-paper">{{ command }}</code>
    <button
      type="button"
      class="grid size-7 shrink-0 place-items-center rounded-md text-smoke transition-colors hover:bg-soot hover:text-paper"
      :aria-label="copied ? 'Copied' : 'Copy command'"
      @click="copy"
    >
      <Check v-if="copied" class="size-4 text-tatami" />
      <Copy v-else class="size-4" />
    </button>
  </div>
</template>
