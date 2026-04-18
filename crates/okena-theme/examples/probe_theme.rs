use okena_theme::load_custom_themes;

fn main() {
    // Forward log warnings to stderr
    struct StderrLogger;
    impl log::Log for StderrLogger {
        fn enabled(&self, _m: &log::Metadata) -> bool { true }
        fn log(&self, r: &log::Record) {
            eprintln!("[{}] {}", r.level(), r.args());
        }
        fn flush(&self) {}
    }
    let _ = log::set_boxed_logger(Box::new(StderrLogger));
    log::set_max_level(log::LevelFilter::Warn);

    let themes = load_custom_themes();
    println!("Loaded {} theme(s)", themes.len());
    for (info, colors) in &themes {
        if !info.id.contains("poimandres-dark") { continue; }
        println!("\n=== {} ({}) ===", info.name, info.id);
        println!("  bg_primary     = #{:06x}", colors.bg_primary);
        println!("  bg_secondary   = #{:06x}", colors.bg_secondary);
        println!("  bg_header      = #{:06x}", colors.bg_header);
        println!("  bg_selection   = #{:06x}", colors.bg_selection);
        println!("  bg_hover       = #{:06x}", colors.bg_hover);
        println!("  border         = #{:06x}", colors.border);
        println!("  border_active  = #{:06x}", colors.border_active);
        println!("  border_focused = #{:06x}", colors.border_focused);
        println!("  border_bell    = #{:06x}", colors.border_bell);
        println!("  border_idle    = #{:06x}", colors.border_idle);
        println!("  text_primary   = #{:06x}", colors.text_primary);
        println!("  text_secondary = #{:06x}", colors.text_secondary);
        println!("  text_muted     = #{:06x}", colors.text_muted);
        println!("  selection_bg   = #{:06x}", colors.selection_bg);
        println!("  selection_fg   = #{:06x}", colors.selection_fg);
        println!("  success        = #{:06x}", colors.success);
        println!("  warning        = #{:06x}", colors.warning);
        println!("  error          = #{:06x}", colors.error);
        println!("  term_red       = #{:06x}", colors.term_red);
        println!("  term_green     = #{:06x}", colors.term_green);
        println!("  diff_added_fg  = #{:06x}", colors.diff_added_fg);
        println!("  diff_removed_fg= #{:06x}", colors.diff_removed_fg);
    }
}
