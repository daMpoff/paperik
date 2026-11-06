//! Text scopes keep edits inside one cell or one uninterrupted body region.
use super::*;

pub(crate) struct TextScope {
    pub paragraphs: Vec<usize>,
    pub picture: bool,
}
impl ViewContent {
    pub(crate) fn owned_bytes(&self) -> usize {
        use std::mem::size_of;
        self.blocks.len() * size_of::<Block>()
            + self
                .blocks
                .iter()
                .map(|block| match block {
                    Block::Table(t) => {
                        t.columns.len() * size_of::<f32>()
                            + t.rows.len() * size_of::<TableRow>()
                            + t.rows
                                .iter()
                                .map(|r| {
                                    r.cells.len() * size_of::<TableCell>()
                                        + r.cells
                                            .iter()
                                            .map(|c| c.paragraphs.len() * size_of::<usize>())
                                            .sum::<usize>()
                                })
                                .sum::<usize>()
                    }
                    _ => 0,
                })
                .sum::<usize>()
    }
    pub(crate) fn scopes(&self, count: usize) -> Result<Vec<TextScope>, String> {
        let mut scopes = Vec::new();
        let mut index = 0;
        while index < self.blocks.len() {
            match &self.blocks[index] {
                Block::Paragraph(_) => {
                    let mut paragraphs = Vec::new();
                    while let Some(Block::Paragraph(p)) = self.blocks.get(index) {
                        paragraphs.push(*p);
                        index += 1;
                    }
                    scopes.push(TextScope {
                        paragraphs,
                        picture: false,
                    });
                    continue;
                }
                Block::Table(table) => {
                    for row in &table.rows {
                        for cell in &row.cells {
                            if cell.paragraphs.is_empty() {
                                return Err("Ячейка должна содержать хотя бы один абзац".into());
                            }
                            scopes.push(TextScope {
                                paragraphs: cell.paragraphs.clone(),
                                picture: false,
                            });
                        }
                    }
                }
                Block::Picture(p) => scopes.push(TextScope {
                    paragraphs: vec![p.paragraph],
                    picture: true,
                }),
            }
            index += 1;
        }
        if !scopes
            .iter()
            .flat_map(|s| s.paragraphs.iter().copied())
            .eq(0..count)
        {
            return Err("Порядок абзацев не совпадает со структурой документа".into());
        }
        Ok(scopes)
    }
    pub(crate) fn check_edit(&self, count: usize, start: usize, end: usize) -> Result<(), String> {
        let scopes = self.scopes(count)?;
        let scope = scopes
            .iter()
            .find(|s| s.paragraphs.contains(&start))
            .ok_or("Не найдена область правки")?;
        if scope.picture {
            return Err(
                "Рисунок защищён от текстовой правки; выберите соседний текстовый абзац".into(),
            );
        }
        if !scope.paragraphs.contains(&end) {
            return Err("Правка пересекает границу ячейки, таблицы или рисунка. Изменяйте текст внутри одной области.".into());
        }
        Ok(())
    }
    /// Rebuild indices from retained source anchors, including new/split paragraphs.
    /// Also used when restoring untrusted checkpoints, not only by the editor.
    pub(crate) fn rebind(
        &self,
        original: &[Paragraph],
        current: &[Paragraph],
    ) -> Result<Self, String> {
        let scopes = self.scopes(original.len())?;
        let mut owners = BTreeMap::new();
        for (owner, scope) in scopes.iter().enumerate() {
            for index in &scope.paragraphs {
                let source = original[*index]
                    .source
                    .ok_or("У абзаца нет привязки к исходнику")?;
                if owners.insert(source, owner).is_some_and(|old| old != owner) {
                    return Err("Исходный абзац связан с разными ячейками".into());
                }
            }
        }
        let mut indices = vec![Vec::new(); scopes.len()];
        let mut previous = 0;
        for (index, p) in current.iter().enumerate() {
            let owner = *p
                .source
                .and_then(|s| owners.get(&s))
                .ok_or("Новый абзац не связан со своей областью")?;
            if owner < previous {
                return Err(
                    "Перемещение текста через границы объектов пока не поддерживается".into(),
                );
            }
            if p.runs
                .iter()
                .filter_map(|r| r.source)
                .any(|(p, _)| owners.get(&p) != Some(&owner))
            {
                return Err(
                    "Оформление фрагмента относится к другой ячейке или области документа".into(),
                );
            }
            previous = owner;
            indices[owner].push(index);
        }
        if indices.iter().any(Vec::is_empty) {
            return Err("Нельзя удалить ячейку или объект текстовой правкой".into());
        }
        for p in original.iter().filter(|p| p.edit_block.is_some()) {
            let matching: Vec<_> = current.iter().filter(|n| n.source == p.source).collect();
            if matching.len() != 1 || matching[0] != p {
                return Err("Изменён защищённый абзац с рисунком или служебными объектами".into());
            }
        }
        let mut output = Vec::new();
        let mut block = 0;
        let mut owner = 0;
        while block < self.blocks.len() {
            match &self.blocks[block] {
                Block::Paragraph(_) => {
                    output.extend(indices[owner].iter().copied().map(Block::Paragraph));
                    owner += 1;
                    while matches!(self.blocks.get(block), Some(Block::Paragraph(_))) {
                        block += 1;
                    }
                    continue;
                }
                Block::Table(table) => {
                    let mut table = table.clone();
                    for row in &mut table.rows {
                        for cell in &mut row.cells {
                            cell.paragraphs = indices[owner].clone();
                            owner += 1;
                        }
                    }
                    output.push(Block::Table(table));
                }
                Block::Picture(picture) => {
                    if indices[owner].len() != 1 {
                        return Err("Нельзя разделить абзац с рисунком".into());
                    }
                    let mut picture = picture.clone();
                    picture.paragraph = indices[owner][0];
                    owner += 1;
                    output.push(Block::Picture(picture));
                }
            }
            block += 1;
        }
        Ok(Self { blocks: output })
    }
}
