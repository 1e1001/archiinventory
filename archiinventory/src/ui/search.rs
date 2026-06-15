use std::cmp::Reverse;

use eframe::egui;

use crate::data::{SlotItemGroup, SlotItemInstance, WorldFileSlot};
use crate::ui::{
	ICON_FILTER_NO, ICON_FILTER_NONE, ICON_FILTER_YES, ICON_SORT_DOWN, ICON_SORT_NONE,
	ICON_SORT_UP, ICON_VIEW_SOME, plural,
};

#[derive(Clone)]
pub struct SlotRowStat {
	name_normal: String,
	pub count: usize,
	pub type_count: [usize; 3],
	pub last_index: usize,
	pub unconfirmed: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SlotSortOrder {
	Name(bool),
	Count(bool),
	Index(bool),
}
impl SlotSortOrder {
	// sort highest count by default
	pub const COUNT: Self = Self::Count(true);
	pub const INDEX: Self = Self::Index(false);
	pub const NAME: Self = Self::Name(false);
}

pub struct SlotSearcher {
	// requires scanning all items
	stale_inventory_or_type_filter: bool,
	// requires refreshing indices
	stale_name_filter_or_sort_order: bool,
	filter_type: [Option<bool>; 3],
	pub filter_name: String,
	sort_order: SlotSortOrder,
	sort_unconfirmed: bool,
	filter_name_normal: String,

	row_stats: Vec<SlotRowStat>,
	row_indices: Vec<usize>,
	last_index: usize,
}

impl Default for SlotSearcher {
	fn default() -> Self {
		Self {
			stale_inventory_or_type_filter: true,
			stale_name_filter_or_sort_order: false,
			filter_type: [None; 3],
			filter_name: String::new(),
			filter_name_normal: String::new(),
			sort_order: SlotSortOrder::Name(false),
			sort_unconfirmed: true,
			row_stats: Vec::new(),
			row_indices: Vec::new(),
			last_index: 0,
		}
	}
}

pub fn normalize_str(text: &str) -> String {
	simd_normalizer::casefold(
		&simd_normalizer::nfkc().normalize(text),
		simd_normalizer::CaseFoldMode::Standard,
	)
	.into_owned()
}

fn type_matches(filter: [Option<bool>; 3], inst: &SlotItemInstance) -> bool {
	let [prog, useful, trap] = inst.type_flags;
	filter[0].is_none_or(|flag| flag == prog)
		&& filter[1].is_none_or(|flag| flag == useful)
		&& filter[2].is_none_or(|flag| flag == trap)
}

fn fuzzy_matches(needle: &str, haystack: &str) -> bool {
	let mut needle_iter = needle.chars().peekable();
	for ch in haystack.chars() {
		match needle_iter.peek() {
			None => return true,
			Some(&c) if ch == c => _ = needle_iter.next(),
			Some(_) => {},
		}
	}
	needle_iter.peek().is_none()
}

//fn fuzzy_matches_spans(needle: &str, haystack: &str, mut span: impl FnMut(Range<usize>, bool)) {
//	let mut needle_iter = needle.chars().peekable();
//	let mut span_start = 0;
//	let mut span_match = false;
//	for (i, ch) in haystack.char_indices() {
//		match needle_iter.peek() {
//			None => {
//				if span_match {
//					span(Range::from(span_start..i), true);
//					span_start = i;
//				}
//				span(Range::from(span_start..needle.len()), false);
//				return;
//			},
//			Some(&c) if ch == c => {
//				if !span_match {
//					if i > 0 {
//						span(Range::from(span_start..i), false);
//					}
//					span_start = i;
//					span_match = true;
//				}
//				needle_iter.next();
//			},
//			Some(_) => {
//				if span_match {
//					span(Range::from(span_start..i), true);
//					span_start = i;
//					span_match = false;
//				}
//			},
//		}
//	}
//	span(Range::from(span_start..haystack.len()), span_match);
//}

impl SlotSearcher {
	fn set_type_filter(&mut self, index: usize, value: Option<bool>) {
		self.stale_inventory_or_type_filter = true;
		self.filter_type[index] = value;
	}

	fn next_type_filter(&mut self, i: usize) {
		self.set_type_filter(i, match self.filter_type[i] {
			None => Some(true),
			Some(true) => Some(false),
			Some(false) => None,
		});
	}

	fn update_name_filter(&mut self) {
		self.stale_name_filter_or_sort_order = true;
		self.filter_name_normal = normalize_str(&self.filter_name);
	}

	fn update_sort_order(&mut self, order: SlotSortOrder) {
		self.stale_name_filter_or_sort_order = true;
		self.sort_order = match (self.sort_order, order) {
			(SlotSortOrder::Name(v), SlotSortOrder::Name(_)) => SlotSortOrder::Name(!v),
			(SlotSortOrder::Count(v), SlotSortOrder::Count(_)) => SlotSortOrder::Count(!v),
			(SlotSortOrder::Index(v), SlotSortOrder::Index(_)) => SlotSortOrder::Index(!v),
			(_, order) => order,
		}
	}

	fn set_sort_unconfirmed(&mut self, unconfirmed: bool) {
		self.stale_name_filter_or_sort_order = true;
		self.sort_unconfirmed = unconfirmed;
	}

	pub fn inventory_changed(&mut self) { self.stale_inventory_or_type_filter = true; }

	fn update(&mut self, inventory: &[SlotItemGroup], confirmed: usize) {
		if self.stale_inventory_or_type_filter {
			self.stale_inventory_or_type_filter = false;
			self.row_stats.clear();
			self.last_index = 0;
			let type_filter = self.filter_type;
			for item in inventory {
				let mut stat = SlotRowStat {
					name_normal: normalize_str(&item.name),
					count: 0,
					type_count: [0, 0, 0],
					last_index: 0,
					unconfirmed: 0,
				};
				for instance in &item.instances {
					if type_matches(type_filter, instance) {
						stat.count += 1;
						stat.type_count[0] += usize::from(instance.type_flags[0]);
						stat.type_count[1] += usize::from(instance.type_flags[1]);
						stat.type_count[2] += usize::from(instance.type_flags[2]);
						stat.last_index = stat.last_index.max(instance.index);
						stat.unconfirmed += usize::from(instance.index >= confirmed);
					}
				}
				self.row_stats.push(stat);
			}
			// continue rebuilding
			self.stale_name_filter_or_sort_order = true;
		}
		if self.stale_name_filter_or_sort_order {
			self.stale_name_filter_or_sort_order = false;
			self.row_indices.clear();
			self.row_indices.extend(self.row_stats.iter().enumerate().filter_map(|(i, stat)| {
				(stat.count > 0 && fuzzy_matches(&self.filter_name_normal, &stat.name_normal))
					.then_some(i)
			}));
			// sort by name by default
			self.row_indices.sort_by_key(|&i| &self.row_stats[i].name_normal);
			match self.sort_order {
				SlotSortOrder::Name(false) => {},
				SlotSortOrder::Name(true) => self.row_indices.reverse(),
				SlotSortOrder::Count(false) => {
					self.row_indices.sort_by_key(|&i| self.row_stats[i].count);
				},
				SlotSortOrder::Count(true) => {
					self.row_indices.sort_by_key(|&i| Reverse(self.row_stats[i].count));
				},
				// these are swapped since index is visually backwards
				SlotSortOrder::Index(false) => {
					self.row_indices.sort_by_key(|&i| Reverse(self.row_stats[i].count));
				},
				SlotSortOrder::Index(true) => {
					self.row_indices.sort_by_key(|&i| self.row_stats[i].last_index);
				},
			}
			if self.sort_unconfirmed {
				// drag all false values (unconfirmed) to the top
				self.row_indices.sort_by_key(|&i| self.row_stats[i].unconfirmed == 0);
			}
		}
	}

	#[expect(clippy::too_many_lines, clippy::shadow_unrelated, reason = "egui")]
	pub fn ui(&mut self, ui: &mut egui::Ui, slot: &mut WorldFileSlot) -> bool {
		//let panel_height = ui.available_size().y;
		let row_height = ui.spacing().interact_size.y;
		let mut changed = false;
		ui.heading(&slot.name);
		ui.horizontal(|ui| {
			ui.label(format!(
				"{} item{}, {} unconfirmed",
				slot.total_items,
				plural(slot.total_items),
				slot.total_items.saturating_sub(slot.confirmed)
			));
			if ui
				.add_enabled(slot.confirmed < slot.total_items, egui::Button::new("Confirm"))
				.clicked()
			{
				slot.confirmed = slot.total_items;
				changed = true;
			}
		});

		self.update(&slot.inventory, slot.confirmed);

		// columns: view, count, new, index, type, name
		egui_extras::TableBuilder::new(ui)
			.striped(true)
			.cell_layout(egui::Layout::default().with_cross_align(egui::Align::RIGHT))
			.columns(egui_extras::Column::auto(), 5)
			.column(egui_extras::Column::remainder())
			.header(row_height, |mut header| {
				header.col(|ui| {
					// match spacing when there are no items
					ui.add_visible(false, egui::Button::new(ICON_VIEW_SOME));
				});
				header.col(|ui| {
					ui.horizontal(|ui| {
						if ui
							.button(match self.sort_order {
								SlotSortOrder::Count(false) => ICON_SORT_UP,
								SlotSortOrder::Count(true) => ICON_SORT_DOWN,
								_ => ICON_SORT_NONE,
							})
							.clicked()
						{
							self.update_sort_order(SlotSortOrder::COUNT);
						}
						ui.label("Count");
					});
				});
				header.col(|ui| {
					ui.horizontal(|ui| {
						if ui
							.button(if self.sort_unconfirmed {
								ICON_SORT_DOWN
							} else {
								ICON_SORT_NONE
							})
							.clicked()
						{
							self.set_sort_unconfirmed(!self.sort_unconfirmed);
						}
						ui.label("New")
					});
				});
				header.col(|ui| {
					ui.horizontal(|ui| {
						if ui
							.button(match self.sort_order {
								SlotSortOrder::Index(false) => ICON_SORT_UP,
								SlotSortOrder::Index(true) => ICON_SORT_DOWN,
								_ => ICON_SORT_NONE,
							})
							.clicked()
						{
							self.update_sort_order(SlotSortOrder::INDEX);
						}
						ui.label("Index");
					});
				});
				header.col(|ui| {
					ui.horizontal(|ui| {
						ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
						for (i, color, name) in [
							(2, egui::Color32::RED, "Trap"),
							(1, egui::Color32::GREEN, "Useful"),
							(0, egui::Color32::BLUE, "Progression"),
						] {
							if ui
								.add(
									egui::Button::new(match self.filter_type[i] {
										Some(false) => ICON_FILTER_NO,
										Some(true) => ICON_FILTER_YES,
										None => ICON_FILTER_NONE,
									})
									.fill(color),
								)
								.on_hover_ui(|ui| {
									ui.label(name);
								})
								.clicked()
							{
								self.next_type_filter(i);
							}
						}
					});
				});
				header.col(|ui| {
					ui.with_layout(egui::Layout::default(), |ui| {
						ui.horizontal(|ui| {
							if ui
								.button(match self.sort_order {
									SlotSortOrder::Name(false) => ICON_SORT_UP,
									SlotSortOrder::Name(true) => ICON_SORT_DOWN,
									_ => ICON_SORT_NONE,
								})
								.clicked()
							{
								self.update_sort_order(SlotSortOrder::NAME);
							}
							if ui
								.add(
									egui::TextEdit::singleline(&mut self.filter_name)
										.hint_text("Item Name"),
								)
								.changed()
							{
								self.update_name_filter();
							}
						});
					});
				});
			})
			.body(|body| {
				// TODO: i value stripes more than the extra rows, but try to get both :)
				//#[expect(
				//	clippy::cast_possible_truncation,
				//	clippy::cast_sign_loss,
				//	reason = "estimate"
				//)]
				//let extra_rows = (panel_height / 40.0) as usize;
				body.rows(row_height, self.row_indices.len(), |mut row| {
					let Some(&index) = self.row_indices.get(row.index()) else {
						for _ in 0..6 {
							row.col(|_| {});
						}
						return;
					};
					let row_stat = &self.row_stats[index];
					let row_item = &slot.inventory[index];
					row.col(|ui| {
						if ui.button(ICON_VIEW_SOME).clicked() {
							todo!("ItemSearcher");
						}
					});
					row.col(|ui| _ = ui.label(format!("{}", row_stat.count)));
					row.col(|ui| {
						ui.label(if row_stat.unconfirmed == 0 {
							String::new()
						} else {
							format!("+{}", row_stat.unconfirmed)
						});
					});
					row.col(|ui| {
						ui.label(format!(
							"#{}",
							slot.total_items.saturating_sub(row_stat.last_index)
						))
						.on_hover_ui(|ui| {
							ui.label(format!(
								"Last received as item {} of {}",
								row_stat.last_index + 1,
								slot.total_items
							));
						});
					});
					row.col(|ui| {
						let res = ui.allocate_response(ui.max_rect().size(), egui::Sense::all());
						#[expect(clippy::cast_possible_truncation, reason = "clamped")]
						let [b, g, r] = row_stat.type_count.map(|stat| ((stat * 255) / row_stat.count) as u8);
						ui.painter().rect_filled(
							res.rect,
							ui.visuals().menu_corner_radius,
							egui::Color32::from_rgb(r, g, b),
						);
						res.on_hover_ui(|ui| {
							let mut shown = false;
							for (name, count) in
								["Progression", "Useful", "Trap"].iter().zip(row_stat.type_count)
							{
								if count > 0 {
									shown = true;
									#[expect(clippy::cast_precision_loss, reason = "estimate")]
									ui.label(format!(
										"{name}: {:.2}% ({count})",
										100.0 * (count as f32 / row_stat.count as f32)
									));
								}
							}
							if !shown {
								ui.label("Completely useless…");
							}
						});
					});
					row.col(|ui| {
						ui.with_layout(egui::Layout::default(), |ui| {
							// TODO: underline parts of name that match search
							// it's way more complicated than just running the fuzzy match, since that's operating on the normalized name!
							//let mut job = LayoutJob {
							//	text: (*row_item.name).into(),
							//	wrap: egui::text::TextWrapping::truncate_at_width(
							//		ui.max_rect().width(),
							//	),
							//	..Default::default()
							//};
							//fuzzy_match_spans()

							ui.label(&*row_item.name)
								.on_hover_ui(|ui| _ = ui.label(format!("ID: {}", row_item.id)));
						});
					});
				});
			});

		changed
	}
}

// TODO: ItemSearcher
