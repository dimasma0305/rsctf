import { Box, Center, Group, Pagination, Paper, ScrollArea, Text, em } from '@mantine/core'
import { useResizeObserver } from '@mantine/hooks'
import { FC, memo, useEffect, useState } from 'react'
import { ErrorBoundary } from 'react-error-boundary'
import { useTranslation } from 'react-i18next'
import { Document, Page, pdfjs } from 'react-pdf'
import 'react-pdf/dist/Page/AnnotationLayer.css'
import 'react-pdf/dist/Page/TextLayer.css'
import { showErrorMsg } from '@Utils/Shared'
import classes from '@Styles/PDFViewer.module.css'

pdfjs.GlobalWorkerOptions.workerSrc = new URL('pdfjs-dist/build/pdf.worker.min.mjs', import.meta.url).toString()

interface PDFViewerProps {
  url?: string
  height?: number | string
  active?: boolean
}

export const PDFViewer: FC<PDFViewerProps> = memo(({ url, height, active = true }) => {
  const [numPages, setNumPages] = useState(0)
  const [page, setPage] = useState(1)
  const [pageWidth, setPageWidth] = useState(400)
  const { t } = useTranslation()

  const h = height ? em(height) : 'calc(100vh - 110px)'
  const [ref, { width }] = useResizeObserver<HTMLDivElement>()
  useEffect(() => {
    // Retain the last visible width: hiding a tab must not resize the PDF to a fallback size.
    if (width > 0) setPageWidth(Math.max(1, Math.floor(width)))
  }, [width])

  return (
    <ErrorBoundary
      fallback={
        <Center mih={h}>
          <Text>{t('admin.content.games.writeups.pdf_fallback')}</Text>
        </Center>
      }
      onError={(e) => showErrorMsg(e, t)}
    >
      <Box
        data-pdf-viewer
        className={classes.box}
        __vars={{
          '--pdf-height': h,
        }}
      >
        {numPages > 0 && (
          <Box component="nav" aria-label={t('admin.grading.pdf_pages', 'Writeup pages')} mb="sm">
            <Pagination.Root
              value={page}
              onChange={setPage}
              total={numPages}
              size="sm"
              siblings={0}
              getItemProps={(value) => ({
                'aria-label': t('admin.grading.pdf_page', 'Page {{page}}', { page: value }),
              })}
            >
              <Group justify="center" gap={4}>
                <Pagination.Previous aria-label={t('admin.grading.pdf_previous', 'Previous writeup page')} />
                <Pagination.Items />
                <Pagination.Next aria-label={t('admin.grading.pdf_next', 'Next writeup page')} />
              </Group>
            </Pagination.Root>
            <Text size="xs" c="dimmed" ta="center" mt={4} role="status">
              {t('admin.grading.pdf_page_of', 'Page {{page}} of {{total}}', { page, total: numPages })}
            </Text>
          </Box>
        )}
        <ScrollArea
          h={h}
          className={classes.layout}
          type="never"
          viewportProps={{
            tabIndex: 0,
            role: 'region',
            'aria-label': t('admin.grading.document', 'Submitted writeup'),
          }}
        >
          <Box ref={ref}>
            <Document
              file={url}
              className={classes.doc}
              onLoadSuccess={({ numPages }) => {
                setNumPages(numPages)
                setPage(1)
              }}
              onLoadError={(e) => showErrorMsg(e, t)}
            >
              {/* Keep the document alive across tabs, but never render hidden or unselected pages. */}
              {active && numPages > 0 && (
                <Paper className={classes.paper} key={page}>
                  <Page
                    width={pageWidth}
                    pageNumber={page}
                    devicePixelRatio={Math.min(window.devicePixelRatio || 1, 2)}
                    renderAnnotationLayer={false}
                  />
                </Paper>
              )}
            </Document>
          </Box>
        </ScrollArea>
      </Box>
    </ErrorBoundary>
  )
})
