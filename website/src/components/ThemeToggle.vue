<script setup lang="ts">
import { Monitor, Moon, Sun } from '@lucide/vue'
import { useTheme, type ThemeChoice } from '@/composables/useTheme'

const { choice } = useTheme()
const options: { value: ThemeChoice; label: string; icon: typeof Sun }[] = [
  { value: 'auto', label: 'Match the system theme', icon: Monitor },
  { value: 'light', label: 'Light theme', icon: Sun },
  { value: 'dark', label: 'Dark theme', icon: Moon },
]
</script>

<template>
  <div role="radiogroup" aria-label="Theme" class="inline-flex rounded-full border border-grain bg-soot p-0.5">
    <button
      v-for="option in options"
      :key="option.value"
      type="button"
      role="radio"
      :aria-checked="choice === option.value"
      :aria-label="option.label"
      :title="option.label"
      class="grid size-8 place-items-center rounded-full text-smoke transition-colors hover:text-paper aria-checked:bg-hearth aria-checked:text-paper"
      @click="choice = option.value"
    >
      <component :is="option.icon" class="size-4" />
    </button>
  </div>
</template>
