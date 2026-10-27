<script setup lang="ts">
import FaqSection from '@/components/FaqSection.vue'
import FeaturesSection from '@/components/FeaturesSection.vue'
import GallerySection from '@/components/GallerySection.vue'
import HeroSection from '@/components/HeroSection.vue'
import InstallSection from '@/components/InstallSection.vue'
import NumbersBand from '@/components/NumbersBand.vue'
import SiteFooter from '@/components/SiteFooter.vue'
import SiteHeader from '@/components/SiteHeader.vue'
import StoresBand from '@/components/StoresBand.vue'
</script>

<template>
  <SiteHeader />
  <main>
    <HeroSection />
    <StoresBand />
    <FeaturesSection />
    <NumbersBand />
    <GallerySection />
    <InstallSection />
    <FaqSection />
  </main>
  <SiteFooter />
</template>
