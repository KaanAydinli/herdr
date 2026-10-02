use ratatui::layout::Rect;

pub(super) struct SidebarSections {
    pub spaces: Rect,
    pub agents: Rect,
    pub graph: Rect,
}

pub(super) fn list_height(heights: &[u16], gaps: &[u16]) -> u16 {
    heights.iter().zip(gaps).fold(0u16, |height, (row, gap)| {
        height.saturating_add(*row).saturating_add(*gap)
    })
}

pub(super) fn sections(
    area: Rect,
    spaces: u16,
    agents: u16,
    minimum_spaces: u16,
    reserve_graph: bool,
) -> SidebarSections {
    let width = area.width.saturating_sub(1);
    let minimum_spaces = spaces.min(minimum_spaces);
    let minimum_agents = agents.min(4);
    let graph_room = area
        .height
        .saturating_sub(minimum_spaces.saturating_add(minimum_agents));
    let graph_minimum = if reserve_graph && graph_room >= 4 {
        graph_room.min(6)
    } else {
        0
    };
    let available = area.height.saturating_sub(graph_minimum);
    let extra = available.saturating_sub(minimum_spaces.saturating_add(minimum_agents));
    let space_extra = (extra / 2).max(extra.saturating_sub(agents.saturating_sub(minimum_agents)));
    let spaces = spaces
        .min(minimum_spaces.saturating_add(space_extra))
        .min(available.saturating_sub(minimum_agents));
    let agents = agents.min(available.saturating_sub(spaces));
    SidebarSections {
        spaces: Rect::new(area.x, area.y, width, spaces),
        agents: Rect::new(area.x, area.y.saturating_add(spaces), width, agents),
        graph: Rect::new(
            area.x,
            area.y.saturating_add(spaces).saturating_add(agents),
            width,
            area.height.saturating_sub(spaces).saturating_sub(agents),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_sized_sections_are_contiguous_and_bounded() {
        for width in 0..40 {
            for height in 0..80 {
                for (spaces, agents) in [(6, 3), (12, 8), (200, 3), (6, 200), (200, 200)] {
                    let area = Rect::new(2, 3, width, height);
                    let layout = sections(area, spaces, agents, 6, true);
                    assert_eq!(layout.spaces.bottom(), layout.agents.y);
                    assert_eq!(layout.agents.bottom(), layout.graph.y);
                    assert_eq!(layout.graph.bottom(), area.bottom());
                    assert!(layout.spaces.height <= spaces);
                    assert!(layout.agents.height <= agents);
                    if spaces + agents + 6 <= height {
                        assert_eq!(layout.spaces.height, spaces);
                        assert_eq!(layout.agents.height, agents);
                    }
                }
            }
        }
    }
}
