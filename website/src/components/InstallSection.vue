<script setup lang="ts">
import { ArrowUpRight } from '@lucide/vue'
import { ref } from 'vue'
import CopyCommand from '@/components/CopyCommand.vue'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { INSTALL, REPO } from '@/data/content'

type Platform = keyof typeof INSTALL

function detect(): Platform {
  const agent = navigator.userAgent
  if (/Windows/i.test(agent)) return 'windows'
  if (/Mac/i.test(agent)) return 'macos'
  return 'linux'
}

const platform = ref<Platform>(detect())
const platforms: { id: Platform; label: string; note: string }[] = [
  { id: 'linux', label: 'Linux', note: 'x86-64. The sign-in window is a separate, recommended package that brings WebKitGTK.' },
  { id: 'macos', label: 'macOS', note: 'macOS 12 or later, on Apple silicon and Intel.' },
  { id: 'windows', label: 'Windows', note: 'Windows 10 and 11, x64. Installs for your user only.' },
]
</script>

<template>
  <section id="download" aria-labelledby="download-title" class="border-y border-grain bg-soot/60">
    <div class="gutter mx-auto grid max-w-7xl gap-12 py-24 lg:grid-cols-[minmax(0,4fr)_minmax(0,8fr)]">
      <div class="flex min-w-0 flex-col gap-4">
        <p class="tag">Download</p>
        <h2 id="download-title" class="font-display text-[clamp(1.9rem,3.6vw,2.8rem)] font-bold leading-[1.08] text-paper">
          Light the fire
        </h2>
        <p class="text-smoke">
          Irori comes from the package manager you already use, and updates with the rest of
          your system.
        </p>
        <div class="flex flex-col gap-2 rounded-xl border border-ember/40 bg-ember/10 p-4 text-[0.95rem]">
          <p class="font-medium text-paper">The first release is on its way.</p>
          <p class="text-smoke">
            These commands work once it’s out. Until then, build Irori from source with Rust 1.95
            or newer:
          </p>
          <CopyCommand command="cargo run --release -p irori" />
        </div>
        <a :href="`${REPO}/releases`" class="inline-flex items-center gap-1 text-paper underline decoration-wood underline-offset-4 hover:decoration-flame">
          All releases <ArrowUpRight class="size-4" />
        </a>
      </div>

      <Tabs v-model="platform" class="min-w-0 gap-6">
        <TabsList class="h-11 w-full rounded-full bg-hearth p-1 sm:w-auto sm:self-start">
          <TabsTrigger v-for="p in platforms" :key="p.id" :value="p.id" class="rounded-full px-5 text-[0.95rem]">{{ p.label }}</TabsTrigger>
        </TabsList>
        <TabsContent v-for="p in platforms" :key="p.id" :value="p.id" class="flex min-w-0 flex-col gap-4">
          <p class="font-mono text-sm text-smoke">{{ p.note }}</p>
          <div class="flex flex-col divide-y divide-grain overflow-hidden rounded-xl border border-grain bg-soot">
            <div v-for="option in INSTALL[p.id]" :key="option.label" class="grid min-w-0 gap-3 p-5 sm:grid-cols-[10rem_minmax(0,1fr)]">
              <h3 class="font-display text-base font-bold text-paper">{{ option.label }}</h3>
              <div class="flex min-w-0 flex-col gap-2">
                <CopyCommand v-for="command in option.commands" :key="command" :command="command" />
                <p v-if="option.note" class="text-[0.95rem] text-smoke">{{ option.note }}</p>
              </div>
            </div>
          </div>
        </TabsContent>
      </Tabs>
    </div>
  </section>
</template>
