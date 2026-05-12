#!/usr/bin/env node
// Syncs the types file and version to nikwebr/react-native-s3-bg-uploader-docs.
// Called by @semantic-release/exec during the publish step.
// Requires env var: DOCS_REPO_TOKEN (GitHub PAT with contents:write on the docs repo)

const fs = require('fs')
const https = require('https')

const VERSION = process.argv[2]
const DOCS_REPO = 'nikwebr/react-native-s3-bg-uploader-docs'
const BRANCH = 'master'
const TOKEN = process.env.DOCS_REPO_TOKEN

if (!VERSION) {
  console.error('Usage: node scripts/sync-docs-repo.js <version>')
  process.exit(1)
}
if (!TOKEN) {
  console.error('Error: DOCS_REPO_TOKEN env var is required')
  process.exit(1)
}

function request(method, path, body) {
  return new Promise((resolve, reject) => {
    const data = body ? JSON.stringify(body) : null
    const req = https.request(
      {
        hostname: 'api.github.com',
        path,
        method,
        headers: {
          Authorization: `Bearer ${TOKEN}`,
          'Content-Type': 'application/json',
          'User-Agent': 'sync-docs-repo',
          Accept: 'application/vnd.github+json',
          ...(data ? { 'Content-Length': Buffer.byteLength(data) } : {}),
        },
      },
      (res) => {
        let raw = ''
        res.on('data', (chunk) => (raw += chunk))
        res.on('end', () => {
          try {
            resolve({ status: res.statusCode, body: JSON.parse(raw) })
          } catch {
            resolve({ status: res.statusCode, body: raw })
          }
        })
      }
    )
    req.on('error', reject)
    if (data) req.write(data)
    req.end()
  })
}

async function getSha(filePath) {
  const res = await request('GET', `/repos/${DOCS_REPO}/contents/${filePath}?ref=${BRANCH}`)
  return res.status === 200 ? res.body.sha : null
}

async function putFile(filePath, content, message) {
  const sha = await getSha(filePath)
  const body = {
    message,
    content: Buffer.from(content).toString('base64'),
    branch: BRANCH,
    ...(sha ? { sha } : {}),
  }
  const res = await request('PUT', `/repos/${DOCS_REPO}/contents/${filePath}`, body)
  if (res.status >= 400) {
    throw new Error(`Failed to update ${filePath}: ${JSON.stringify(res.body)}`)
  }
  console.log(`  ✓ ${filePath}`)
}

async function triggerWorkflow(workflowFile) {
  const res = await request(
    'POST',
    `/repos/${DOCS_REPO}/actions/workflows/${workflowFile}/dispatches`,
    { ref: BRANCH }
  )
  if (res.status >= 400) {
    throw new Error(`Failed to trigger ${workflowFile}: ${JSON.stringify(res.body)}`)
  }
  console.log(`  ✓ Triggered ${workflowFile}`)
}

async function main() {
  console.log(`Syncing docs repo for v${VERSION}...`)

  const typesContent = fs.readFileSync('src/specs/s3-bg-uploader.types.ts', 'utf-8')
  await putFile(
    'lib/s3-bg-uploader.types.ts',
    typesContent,
    `chore: update types for v${VERSION} [skip ci]`
  )

  const sharedContent = `export const version = '${VERSION}'\n`
  await putFile('lib/version.ts', sharedContent, `chore: bump version to v${VERSION} [skip ci]`)

  await triggerWorkflow('deploy.yml')

  console.log(`✅ Done`)
}

main().catch((err) => {
  console.error(err)
  process.exit(1)
})
