<script setup lang="ts">
import HearthFrame from '@/components/HearthFrame.vue'
import ScreenImage from '@/components/ScreenImage.vue'
import { SCREENS, SYSTEMS } from '@/data/content'

const screen = (id: string) => SCREENS.find((s) => s.id === id)!

const chapters = [
  {
    tag: 'Library',
    title: 'One library for every store',
    body: 'Your store libraries sit beside your installed Steam games, your ROMs and anything you add yourself. Search, filter, sort and group them, and pick up where you left off.',
    points: [
      'Fuzzy search, and a command palette on <kbd>Ctrl</kbd> <kbd>K</kbd>',
      'Favourites, categories, hidden games and a random pick on <kbd>Ctrl</kbd> <kbd>Shift</kbd> <kbd>G</kbd>',
      'Grid and list views that stay smooth with thousands of games',
    ],
    screen: screen('library'),
  },
  {
    tag: 'Play',
    title: 'One click to play',
    body: 'Irori fetches Wine, Proton, umu, DXVK and VKD3D-Proton for you, keeps a prefix per game, and syncs cloud saves before and after you play.',
    points: [
      'GE-Proton, Proton-CachyOS, Wine-GE and Kron4ek builds on Linux',
      'Wine, CrossOver bottles and Game Porting Toolkit on macOS',
      'Cloud saves for Epic Games and GOG, and a choice when both copies changed',
    ],
    screen: screen('game'),
  },
  {
    tag: 'Downloads',
    title: 'Downloads that finish',
    body: 'Downloads run in parallel chunks and verify as they go. They pause when you go offline and carry on after a restart, and updates fetch only what changed.',
    points: [
      'Reorder, pause and cancel the queue',
      'Repair checks every file against the store’s manifest',
      'Update all your games at once',
    ],
    screen: screen('downloads'),
  },
  {
    tag: 'Emulators',
    title: 'Emulators, set up once',
    body: 'Point Irori at your ROM folders. It recognizes each system, installs the emulator you choose from the project’s own servers, links your BIOS files and starts the game.',
    points: [
      'RetroArch with twelve cores, DuckStation, PCSX2, Dolphin, PPSSPP, melonDS, mGBA, Azahar, ScummVM and DOSBox Staging',
      'BIOS files checked before a game starts, never downloaded',
    ],
    systems: true,
    screen: screen('emulators'),
  },
  {
    tag: 'Big Picture',
    title: 'Made for the couch',
    body: 'Big Picture runs the whole launcher with a controller. It opens by itself in the Steam Deck’s Game Mode and scales up to 200 % for a TV.',
    points: [
      'Xbox, PlayStation and DualSense, Switch Pro and Steam Deck controllers',
      'Hot-plugging and battery levels',
      'The Guide button opens and closes it from anywhere',
    ],
    screen: screen('bigpicture'),
  },
]
</script>

<template>
  <section id="features" aria-label="Features" class="gutter mx-auto flex max-w-7xl flex-col gap-28 py-24 lg:gap-36 lg:py-32">
    <article
      v-for="(chapter, index) in chapters"
      :key="chapter.tag"
      class="grid items-center gap-x-14 gap-y-10"
      :class="index % 2 ? 'lg:grid-cols-[minmax(0,7fr)_minmax(0,5fr)]' : 'lg:grid-cols-[minmax(0,5fr)_minmax(0,7fr)]'"
    >
      <div class="flex min-w-0 flex-col gap-5" :class="index % 2 ? 'lg:order-2' : ''">
        <p class="tag">{{ chapter.tag }}</p>
        <h2 class="font-display text-[clamp(1.9rem,3.6vw,2.8rem)] font-bold leading-[1.08] text-paper">{{ chapter.title }}</h2>
        <p class="max-w-[34rem] text-smoke">{{ chapter.body }}</p>
        <ul class="flex max-w-[34rem] flex-col border-t border-grain">
          <!-- Copy is written in this file, never user input. -->
          <li v-for="point in chapter.points" :key="point" class="border-b border-grain py-3 text-[0.97rem] text-paper" v-html="point" />
        </ul>
        <ul v-if="chapter.systems" class="flex max-w-[34rem] flex-wrap gap-1.5" aria-label="Systems">
          <li v-for="system in SYSTEMS" :key="system" class="rounded-md border border-grain bg-soot px-2 py-0.5 font-mono text-xs text-smoke">{{ system }}</li>
        </ul>
      </div>
      <HearthFrame class="min-w-0">
        <ScreenImage :screen="chapter.screen" />
      </HearthFrame>
    </article>
  </section>
</template>
