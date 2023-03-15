pub use crate::dir::*;
use anyhow::{anyhow, bail, Result};
use std::path::{Path, PathBuf};

use tui_tree_widget::{TreeItem, TreeState};

#[derive(Debug)]
pub struct Files<'a> {
    items: Vec<TreeItem<'a>>,
    dir: Dir,
}

impl<'a> Files<'a> {
    pub fn remove_file(&mut self, location: &[usize]) -> Result<Item> {
        if location.len() == 1 {
            let item = self.dir.remove_child(location[0])?;
            self.update();
            return Ok(item);
        }
        let item = if let Item::Dir(dir) = self
            .dir
            .nested_child_mut(&location[..location.len() - 1])
            .ok_or(anyhow!("could not remove file: invalid location"))?
        {
            dir.remove_child(location[location.len() - 1])?
        } else {
            bail!("could not remove file: invalid location")
        };
        self.update();
        Ok(item)
    }

    pub fn add_file(&mut self, location: &[usize], name: &str) -> Result<&File> {
        const MESSAGE: &str = "could not add file: invalid location";

        if let Item::Dir(dir) = self
            .dir
            .nested_child_mut(location)
            .ok_or(anyhow!(MESSAGE))?
        {
            dir.new_file(name)?;
        } else {
            bail!(MESSAGE)
        };
        self.update();
        let child = if let Item::Dir(dir) = self
            .dir
            .nested_child(location)
            .expect("path should be valid by by this point")
        {
            if let Item::File(file) = dir
                .iter()
                .find(|child| last_of_path(child.path()) == name)
                .expect("file should be in directory")
            {
                file
            } else {
                unreachable!("path must lead to file")
            }
        } else {
            unreachable!("path cannot be a dir at this point")
        };
        Ok(child)
    }

    pub fn items(&self) -> &[TreeItem] {
        self.items.as_ref()
    }

    fn update(&mut self) {
        self.items = build_filetree(&self.dir);
    }
}

#[derive(Debug)]
pub struct Filetree<'a> {
    pub state: TreeState,
    pub files: Files<'a>,
    root_path: PathBuf,
}

impl<'a> Filetree<'a> {
    pub fn from_dir(path: impl AsRef<Path>) -> Result<Self> {
        let tree = DirBuilder::new(&path).build()?;
        let file_tree = build_filetree(&tree);
        let mut state = TreeState::default();
        state.select_first();
        Ok(Filetree {
            root_path: path.as_ref().to_path_buf(),
            state,
            files: Files {
                items: file_tree,
                dir: tree,
            },
        })
    }

    pub fn first(&mut self) {
        self.state.select_first();
    }

    pub fn last(&mut self) {
        self.state.select_last(&self.files.items);
    }

    pub fn toggle(&mut self) {
        self.state.toggle_selected();
    }

    pub fn down(&mut self) {
        self.state.key_down(&self.files.items);
    }

    pub fn up(&mut self) {
        self.state.key_up(&self.files.items);
    }

    pub fn get_node(&self, place: &[usize]) -> Option<&Item> {
        let mut places = place.iter();
        let mut node = self.files.dir.child(*places.next()?)?;
        for idx in places {
            node = match node {
                Item::Dir(dir) => dir.child(*idx)?,
                // Path goes to file, invalid
                Item::File(_) => return None,
            };
        }
        Some(node)
    }

    pub fn get_selected(&self) -> &Item {
        self.get_node(&self.state.selected())
            .expect("selected should be in tree")
    }

    pub fn remove_file(&mut self, location: &[usize]) -> Result<Item> {
        let item = self.files.remove_file(location)?;
        // Prevents opening next selected item
        self.state.close(&self.state.selected());
        Ok(item)
    }

    pub fn add_file(&mut self, location: &[usize], name: &str) -> Result<()> {
        self.files.add_file(location, name)?;
        Ok(())
    }

    pub fn remove_selected(&mut self) -> Result<Item> {
        self.remove_file(&self.state.selected())
    }
}

fn last_of_path(path: impl AsRef<Path>) -> String {
    path.as_ref()
        .iter()
        .last()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}

fn build_filetree<'a>(tree: &Dir) -> Vec<TreeItem<'a>> {
    let mut items = Vec::new();
    for item in tree {
        let tree_item = match item {
            Item::Dir(dir) => TreeItem::new(last_of_path(dir.path()), build_filetree(dir)),
            Item::File(file) => TreeItem::new_leaf(last_of_path(file.path())),
        };
        items.push(tree_item);
    }
    items
}
