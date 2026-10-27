<script setup lang="ts">
import { ChevronLeft, ChevronRight } from '@lucide/vue'
import { computed, ref, watch } from 'vue'
import ScreenImage from '@/components/ScreenImage.vue'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogTitle } from '@/components/ui/dialog'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useTheme } from '@/composables/useTheme'
import { SCREENS } from '@/data/content'

const { isDark } = useTheme()
const variant = ref<'dark' | 'light'>(isDark.value ? 'dark' : 'light')
watch(isDark, (dark) => (variant.value = dark ? 'dark' : 'light'))

const screens = computed(() => (variant.value === 'light' ? SCREENS.filter((s) => s.light) : SCREENS))
const open = ref<number | null>(null)
const current = computed(() => (open.value === null ? null : screens.value[open.value]))

function step(by: number) {
  if (open.value === null) return
  const count = screens.value.length
  open.value = (open.value + by + count) % count
}

function onKey(event: KeyboardEvent) {
  if (event.key === 'ArrowRight') step(1)
  if (event.key === 'ArrowLeft') step(-1)
}
</script>

<template>
  <section id="screenshots" aria-labelledby="screens-title" class="gutter mx-auto flex max-w-7xl flex-col gap-10 py-24">
    <div class="flex flex-col gap-6 sm:flex-row sm:items-end sm:justify-between">
      <div class="flex max-w-2xl flex-col gap-3">
        <p class="tag">Screenshots</p>
        <h2 id="screens-title" class="font-display text-[clamp(1.9rem,3.6vw,2.8rem)] font-bold leading-[1.08] text-paper">
          Look around the room
        </h2>
        <p class="text-smoke">
          Every screen as it runs on macOS. The games are made up for this page; their covers
          were drawn for it too.
        </p>
      </div>
      <Tabs v-model="variant">
        <TabsList class="h-10 rounded-full bg-soot p-1">
          <TabsTrigger value="dark" class="rounded-full px-4">Dark</TabsTrigger>
          <TabsTrigger value="light" class="rounded-full px-4">Light</TabsTrigger>
        </TabsList>
      </Tabs>
    </div>

    <ul class="grid grid-cols-1 gap-x-6 gap-y-10 sm:grid-cols-2 lg:grid-cols-3">
      <li v-for="(screen, index) in screens" :key="`${variant}-${screen.id}`" class="min-w-0">
        <button
          type="button"
          class="group flex w-full flex-col gap-3 text-left"
          :aria-label="`Open the ${screen.title} screenshot`"
          @click="open = index"
        >
          <span class="block overflow-hidden rounded-xl border border-grain bg-soot transition-[transform,box-shadow] duration-300 group-hover:-translate-y-1 group-hover:shadow-[0_24px_50px_-28px_var(--glow)]">
            <ScreenImage :screen="screen" :variant="variant" sizes="(min-width: 1024px) 30vw, (min-width: 640px) 50vw, 100vw" />
          </span>
          <span class="flex flex-col gap-0.5">
            <span class="font-display text-lg font-bold text-paper">{{ screen.title }}</span>
            <span class="text-[0.93rem] leading-snug text-smoke">{{ screen.caption }}</span>
          </span>
        </button>
      </li>
    </ul>

    <Dialog :open="current !== null" @update:open="(value: boolean) => { if (!value) open = null }">
      <DialogContent
        v-if="current"
        class="max-w-[min(96vw,1600px)] gap-4 border-grain bg-ash p-3 sm:max-w-[min(96vw,1600px)] sm:p-4"
        @keydown="onKey"
      >
        <ScreenImage :screen="current" :variant="variant" eager sizes="96vw" class="rounded-lg" />
        <div class="flex flex-wrap items-center gap-3 px-1 pb-1">
          <div class="flex min-w-0 flex-1 flex-col">
            <DialogTitle class="font-display text-lg font-bold text-paper">{{ current.title }}</DialogTitle>
            <DialogDescription class="text-smoke">{{ current.caption }}</DialogDescription>
          </div>
          <span class="font-mono text-sm tabular-nums text-smoke">{{ (open ?? 0) + 1 }} / {{ screens.length }}</span>
          <Button variant="outline" size="icon" class="rounded-full" aria-label="Previous screenshot" @click="step(-1)">
            <ChevronLeft />
          </Button>
          <Button variant="outline" size="icon" class="rounded-full" aria-label="Next screenshot" @click="step(1)">
            <ChevronRight />
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  </section>
</template>
