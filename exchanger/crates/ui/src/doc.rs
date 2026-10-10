//! Документ сразу в двух видах — rich-HTML для `sendRichMessage` и обычный HTML-фолбэк
//! (порт lovec `format.rs:786-1096`, `Doc`/`List`/`Table`). Только для справочных экранов:
//! денежный поток — обычный HTML (SPEC §6.1).
//!
//! Соответствие блоков:
//! | rich                          | обычный HTML                                  |
//! |-------------------------------|-----------------------------------------------|
//! | `<h3>`, `<h4>`                | жирная строка                                 |
//! | `<p>`                         | строка                                        |
//! | `<ul><li>`                    | строки `• …`                                  |
//! | `<table>`                     | готовая фраза на строку, при желании в цитате |
//! | `<details><summary>`          | `<blockquote expandable>` с жирной подписью   |
//! | `<footer>`                    | курсив                                        |
//!
//! Вложенных цитат Telegram не допускает: внутри `details` цитата становится курсивом
//! (флаг `quoted`, как у lovec). Перед `</blockquote>` не остаётся пустой строки.

use crate::html::Frag;

pub(crate) struct Doc {
    rich: String,
    plain: String,
    /// Обычный HTML сейчас внутри цитаты.
    quoted: bool,
}

/// Строка таблицы: ячейки для rich и готовая фраза для обычного HTML.
pub(crate) struct Row {
    pub cells: Vec<Frag>,
    pub line: Frag,
}

pub(crate) struct Table<'a> {
    /// Атрибуты `<table>`: `bordered`, `striped`, `compact`.
    pub attrs: &'a str,
    pub head: Vec<Frag>,
    pub rows: Vec<Row>,
    /// Колонки с числами — прижать вправо.
    pub right: &'a [usize],
    /// В обычном HTML обернуть строки в цитату.
    pub plain_quote: bool,
}

impl Doc {
    pub fn new() -> Self {
        Self {
            rich: String::with_capacity(1536),
            plain: String::with_capacity(768),
            quoted: false,
        }
    }

    /// `(rich, plain)`. Хвостовые пробелы и переводы строк обычного HTML срезаются.
    pub fn finish(mut self) -> (String, String) {
        let len = self.plain.trim_end().len();
        self.plain.truncate(len);
        (self.rich, self.plain)
    }

    pub fn title(&mut self, title: &Frag) {
        self.rich.push_str("<h3>");
        self.rich.push_str(title.rich());
        self.rich.push_str("</h3>");
        self.plain.push_str("<b>");
        self.plain.push_str(title.plain());
        self.plain.push_str("</b>\n");
    }

    pub fn subtitle(&mut self, title: &Frag) {
        self.rich.push_str("<h4>");
        self.rich.push_str(title.rich());
        self.rich.push_str("</h4>");
        self.plain.push_str("<b>");
        self.plain.push_str(title.plain());
        self.plain.push_str("</b>\n");
    }

    pub fn para(&mut self, text: &Frag) {
        self.rich.push_str("<p>");
        self.rich.push_str(text.rich());
        self.rich.push_str("</p>");
        self.plain.push_str(text.plain());
        self.plain.push('\n');
    }

    /// Пустая строка между разделами — только в обычном HTML.
    pub fn gap(&mut self) {
        self.plain.push('\n');
    }

    pub fn list(&mut self, items: &[Frag]) {
        self.rich.push_str("<ul>");
        for item in items {
            self.rich.push_str("<li>");
            self.rich.push_str(item.rich());
            self.rich.push_str("</li>");
            self.plain.push_str("• ");
            self.plain.push_str(item.plain());
            self.plain.push('\n');
        }
        self.rich.push_str("</ul>");
    }

    /// Раскрывающийся блок; в обычном HTML — свёрнутая цитата (раскрытая, если `open`).
    pub fn details(&mut self, summary: &Frag, open: bool, body: impl FnOnce(&mut Doc)) {
        self.rich.push_str(if open {
            "<details open><summary>"
        } else {
            "<details><summary>"
        });
        self.rich.push_str(summary.rich());
        self.rich.push_str("</summary>");
        let nested = self.quoted;
        if !nested {
            self.plain.push_str(if open {
                "<blockquote>"
            } else {
                "<blockquote expandable>"
            });
        }
        self.plain.push_str("<b>");
        self.plain.push_str(summary.plain());
        self.plain.push_str("</b>\n");
        self.quoted = true;
        body(self);
        self.quoted = nested;
        self.rich.push_str("</details>");
        if !nested {
            while self.plain.ends_with('\n') {
                self.plain.pop();
            }
            self.plain.push_str("</blockquote>\n");
        }
    }

    pub fn table(&mut self, table: Table<'_>) {
        if table.attrs.is_empty() {
            self.rich.push_str("<table>");
        } else {
            self.rich.push_str("<table ");
            self.rich.push_str(table.attrs);
            self.rich.push('>');
        }
        if !table.head.is_empty() {
            self.rich.push_str("<tr>");
            for (i, cell) in table.head.iter().enumerate() {
                self.rich
                    .push_str(cell_open(true, table.right.contains(&i)));
                self.rich.push_str(cell.rich());
                self.rich.push_str("</th>");
            }
            self.rich.push_str("</tr>");
        }
        for row in &table.rows {
            self.rich.push_str("<tr>");
            for (i, cell) in row.cells.iter().enumerate() {
                self.rich
                    .push_str(cell_open(false, table.right.contains(&i)));
                self.rich.push_str(cell.rich());
                self.rich.push_str("</td>");
            }
            self.rich.push_str("</tr>");
        }
        self.rich.push_str("</table>");

        let lines: Vec<&str> = table.rows.iter().map(|r| r.line.plain()).collect();
        if table.plain_quote && !self.quoted {
            self.plain.push_str("<blockquote>");
            self.plain.push_str(&lines.join("\n"));
            self.plain.push_str("</blockquote>\n");
        } else {
            for line in lines {
                self.plain.push_str(line);
                self.plain.push('\n');
            }
        }
    }

    pub fn footer(&mut self, text: &Frag) {
        self.rich.push_str("<footer>");
        self.rich.push_str(text.rich());
        self.rich.push_str("</footer>");
        self.plain.push_str("<i>");
        self.plain.push_str(text.plain());
        self.plain.push_str("</i>\n");
    }
}

const fn cell_open(header: bool, right: bool) -> &'static str {
    match (header, right) {
        (true, true) => "<th align=\"right\">",
        (true, false) => "<th>",
        (false, true) => "<td align=\"right\">",
        (false, false) => "<td>",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn details_become_expandable_quote_without_trailing_newline() {
        let mut doc = Doc::new();
        doc.title(&Frag::text("Заголовок <x>"));
        doc.details(&Frag::text("Подробнее"), false, |d| {
            d.list(&[Frag::text("раз"), Frag::text("два")]);
            // Таблица с цитатой в обычном HTML внутри цитаты — без вложенной цитаты.
            d.table(Table {
                attrs: "",
                head: Vec::new(),
                rows: vec![Row {
                    cells: vec![Frag::text("ячейка")],
                    line: Frag::text("строка"),
                }],
                right: &[],
                plain_quote: true,
            });
        });
        let (rich, plain) = doc.finish();
        assert_eq!(
            rich,
            "<h3>Заголовок &lt;x&gt;</h3><details><summary>Подробнее</summary>\
             <ul><li>раз</li><li>два</li></ul><table><tr><td>ячейка</td></tr></table></details>"
        );
        assert_eq!(
            plain,
            "<b>Заголовок &lt;x&gt;</b>\n<blockquote expandable><b>Подробнее</b>\n• раз\n• два\n\
             строка</blockquote>"
        );
    }

    #[test]
    fn table_has_rich_cells_and_plain_lines() {
        let mut doc = Doc::new();
        doc.table(Table {
            attrs: "bordered compact",
            head: vec![Frag::text("Что"), Frag::text("%")],
            rows: vec![Row {
                cells: vec![Frag::text("Чек"), Frag::text("2,5")],
                line: Frag::text("Чек · 2,5"),
            }],
            right: &[1],
            plain_quote: true,
        });
        let (rich, plain) = doc.finish();
        assert_eq!(
            rich,
            "<table bordered compact><tr><th>Что</th><th align=\"right\">%</th></tr>\
             <tr><td>Чек</td><td align=\"right\">2,5</td></tr></table>"
        );
        assert_eq!(plain, "<blockquote>Чек · 2,5</blockquote>");
    }
}
