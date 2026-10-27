<script setup lang="ts">
import { Accordion, AccordionContent, AccordionItem, AccordionTrigger } from '@/components/ui/accordion'
import { FAQ } from '@/data/content'
</script>

<template>
  <section id="faq" aria-labelledby="faq-title" class="gutter mx-auto grid max-w-7xl gap-10 py-24 lg:grid-cols-[minmax(0,4fr)_minmax(0,8fr)]">
    <div class="flex flex-col gap-3">
      <p class="tag">Questions</p>
      <h2 id="faq-title" class="font-display text-[clamp(1.9rem,3.6vw,2.8rem)] font-bold leading-[1.08] text-paper">
        Before you sit down
      </h2>
    </div>
    <Accordion type="single" collapsible :default-value="FAQ[0]?.q" class="min-w-0 border-t border-grain">
      <AccordionItem v-for="item in FAQ" :key="item.q" :value="item.q" class="border-grain">
        <AccordionTrigger class="py-5 font-display text-lg font-bold text-paper hover:no-underline">{{ item.q }}</AccordionTrigger>
        <AccordionContent class="max-w-[42rem] pb-6 text-base leading-relaxed text-smoke">{{ item.a }}</AccordionContent>
      </AccordionItem>
    </Accordion>
  </section>
</template>
