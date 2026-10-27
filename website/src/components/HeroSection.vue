<script setup lang="ts">
import { ArrowDown, ArrowUpRight } from '@lucide/vue'
import HearthFrame from '@/components/HearthFrame.vue'
import ScreenImage from '@/components/ScreenImage.vue'
import { Button } from '@/components/ui/button'
import { REPO, SCREENS } from '@/data/content'

const library = SCREENS[0]!
</script>

<template>
  <section id="top" class="hero relative overflow-hidden">
    <div class="gutter mx-auto grid max-w-7xl items-center gap-x-12 gap-y-14 pb-20 pt-14 lg:grid-cols-[minmax(0,5fr)_minmax(0,7fr)] lg:pb-28 lg:pt-20">
      <div class="flex min-w-0 flex-col gap-7">
        <p class="entry" lang="ja">
          <ruby class="font-display text-2xl text-paper">囲<rt>i</rt>炉<rt>ro</rt>裏<rt>ri</rt></ruby>
          <span class="text-smoke" lang="en">
            <em class="not-italic text-tatami">noun.</em>
            The sunken hearth of a Japanese house; the place everyone gathers.
          </span>
        </p>
        <h1 class="font-display text-[clamp(2.6rem,6vw,4.6rem)] font-bold leading-[1.02] text-paper">
          Every game you own, around one fire.
        </h1>
        <p class="max-w-[34rem] text-lg text-smoke">
          A native launcher for Linux, macOS and Windows. Your Epic Games, GOG, Amazon Games,
          ZOOM Platform and itch.io libraries, your Steam games, your emulated games and the ones
          you add yourself, in one calm library that opens instantly and stays out of your way.
        </p>
        <div class="flex flex-wrap items-center gap-3">
          <Button as="a" href="#download" size="lg" class="h-12 rounded-full px-6 text-base">
            Download Irori <ArrowDown class="size-4" />
          </Button>
          <Button as="a" :href="REPO" size="lg" variant="ghost" class="h-12 rounded-full px-5 text-base text-paper">
            Source on GitHub <ArrowUpRight class="size-4" />
          </Button>
        </div>
        <p class="font-mono text-sm text-smoke">
          Free and open source · GPL-3.0 · Rust and GPUI
        </p>
      </div>
      <div class="hero-shot min-w-0 lg:-mr-24 xl:-mr-40">
        <HearthFrame>
          <ScreenImage :screen="library" eager sizes="(min-width: 1024px) 64vw, 100vw" />
        </HearthFrame>
      </div>
    </div>
  </section>
</template>

<style scoped>
.hero::before {
  /* The fire's glow, low and to the right where the hearth sits. */
  content: "";
  position: absolute;
  inset: auto -20% -40% 30%;
  height: 90%;
  background: radial-gradient(closest-side, var(--glow), transparent);
  pointer-events: none;
}
.entry {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: 0.35rem 1rem;
  padding-bottom: 1.1rem;
  border-bottom: 1px solid var(--grain);
  max-width: 34rem;
}
.entry rt {
  font-family: var(--font-code);
  font-size: 0.68rem;
  letter-spacing: 0.08em;
  color: var(--tatami);
}
.entry span {
  font-size: 0.95rem;
  line-height: 1.5;
}
.hero-shot {
  animation: rise 900ms cubic-bezier(0.2, 0.7, 0.2, 1) both;
}
@keyframes rise {
  from { transform: translateY(28px); }
  to { transform: none; }
}
</style>
