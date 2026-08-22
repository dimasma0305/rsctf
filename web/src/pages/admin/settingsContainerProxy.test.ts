import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

test('container settings expose Platform Proxy and explain the VPN override', () => {
  const settings = readFileSync('src/pages/admin/Settings.tsx', 'utf8')
  assert.match(settings, /ContainerPortMappingType\.PlatformProxy/)
  assert.match(settings, /ContainerPortMappingType\.Default/)
  assert.match(settings, /setContainerProvider/)
  assert.match(settings, /containerProvider,/)

  for (const localePath of ['src/locales/en-US/admin.json', 'src/locales/id-ID/admin.json']) {
    const locale = JSON.parse(readFileSync(localePath, 'utf8'))
    const provider = locale.content.settings.container.provider
    assert.match(provider.port_mapping_description, /VPN/i)
    assert.match(provider.platform_proxy, /Platform Proxy/i)
  }
})
