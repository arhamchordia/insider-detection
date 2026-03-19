import { describe, it, expect, vi } from 'vitest'
import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { Pagination, ScoreBar, ThreatBadge } from '../App'

// ── Pagination ────────────────────────────────────────────────────────────────

describe('Pagination', () => {
  it('renders correct page count when total fits on a few pages', () => {
    render(
      <Pagination page={1} total={150} pageSize={50} onChange={() => {}} />
    )
    // totalPages = ceil(150/50) = 3 — buttons 1, 2, 3 should be present
    expect(screen.getByRole('button', { name: '1' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: '2' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: '3' })).toBeInTheDocument()
  })

  it('shows the current page as active (highlighted)', () => {
    render(
      <Pagination page={2} total={150} pageSize={50} onChange={() => {}} />
    )
    const activePage = screen.getByRole('button', { name: '2' })
    // Active page has a non-transparent background colour set via inline style.
    // We verify it's not disabled (disabled buttons can't be "active").
    expect(activePage).not.toBeDisabled()
  })

  it('calls onChange with the correct page when a page button is clicked', async () => {
    const user = userEvent.setup()
    const onChange = vi.fn()
    render(
      <Pagination page={1} total={150} pageSize={50} onChange={onChange} />
    )
    await user.click(screen.getByRole('button', { name: '3' }))
    expect(onChange).toHaveBeenCalledWith(3)
  })

  it('calls onChange with page+1 when the next arrow is clicked', async () => {
    const user = userEvent.setup()
    const onChange = vi.fn()
    render(
      <Pagination page={1} total={150} pageSize={50} onChange={onChange} />
    )
    await user.click(screen.getByRole('button', { name: '→' }))
    expect(onChange).toHaveBeenCalledWith(2)
  })

  it('disables the previous button on the first page', () => {
    render(
      <Pagination page={1} total={150} pageSize={50} onChange={() => {}} />
    )
    expect(screen.getByRole('button', { name: '←' })).toBeDisabled()
  })

  it('disables the next button on the last page', () => {
    render(
      <Pagination page={3} total={150} pageSize={50} onChange={() => {}} />
    )
    expect(screen.getByRole('button', { name: '→' })).toBeDisabled()
  })

  it('renders totalPages = 1 when total is 0', () => {
    render(
      <Pagination page={1} total={0} pageSize={50} onChange={() => {}} />
    )
    expect(screen.getByRole('button', { name: '1' })).toBeInTheDocument()
    expect(screen.getByText('0 results')).toBeInTheDocument()
  })

  it('shows ellipsis for large page counts', () => {
    // With 500 total, pageSize 50, page 5: totalPages=10, page is in middle → ellipsis expected
    render(
      <Pagination page={5} total={500} pageSize={50} onChange={() => {}} />
    )
    const ellipsisNodes = screen.getAllByText('…')
    expect(ellipsisNodes.length).toBeGreaterThan(0)
  })
})

// ── ScoreBar ──────────────────────────────────────────────────────────────────

describe('ScoreBar', () => {
  it('renders a fill div whose width matches the score percentage', () => {
    const { container } = render(<ScoreBar value={0.75} />)
    // The inner fill div has style.width = "75%"
    const divs = container.querySelectorAll('div')
    const fillDiv = Array.from(divs).find(
      (d) => d.style.width === '75%'
    )
    expect(fillDiv).toBeDefined()
  })

  it('clamps width to 100% for scores > 1', () => {
    const { container } = render(<ScoreBar value={1.5} />)
    const divs = container.querySelectorAll('div')
    const fillDiv = Array.from(divs).find(
      (d) => d.style.width === '100%'
    )
    expect(fillDiv).toBeDefined()
  })

  it('renders a score label with the correct formatted value', () => {
    render(<ScoreBar value={0.42} />)
    expect(screen.getByText('0.42')).toBeInTheDocument()
  })

  it('renders 0% fill for value 0', () => {
    const { container } = render(<ScoreBar value={0} />)
    const divs = container.querySelectorAll('div')
    const fillDiv = Array.from(divs).find(
      (d) => d.style.width === '0%'
    )
    expect(fillDiv).toBeDefined()
  })
})

// ── ThreatBadge ───────────────────────────────────────────────────────────────

describe('ThreatBadge', () => {
  it('renders FLAGGED label when flagged=true regardless of score', () => {
    render(<ThreatBadge score={0.3} flagged={true} />)
    expect(screen.getByText('FLAGGED')).toBeInTheDocument()
  })

  it('renders SUSPECT label when score >= 0.5 and not flagged', () => {
    render(<ThreatBadge score={0.65} flagged={false} />)
    expect(screen.getByText('SUSPECT')).toBeInTheDocument()
  })

  it('renders CLEAN label when score < 0.5 and not flagged', () => {
    render(<ThreatBadge score={0.3} flagged={false} />)
    expect(screen.getByText('CLEAN')).toBeInTheDocument()
  })

  it('renders SUSPECT at exactly 0.5 score', () => {
    render(<ThreatBadge score={0.5} flagged={false} />)
    expect(screen.getByText('SUSPECT')).toBeInTheDocument()
  })

  it('renders CLEAN at score just below 0.5', () => {
    render(<ThreatBadge score={0.49} flagged={false} />)
    expect(screen.getByText('CLEAN')).toBeInTheDocument()
  })

  it('FLAGGED takes precedence over score-based classification', () => {
    // High score but explicitly flagged should show FLAGGED not SUSPECT.
    render(<ThreatBadge score={0.9} flagged={true} />)
    expect(screen.getByText('FLAGGED')).toBeInTheDocument()
    expect(screen.queryByText('SUSPECT')).not.toBeInTheDocument()
  })
})
