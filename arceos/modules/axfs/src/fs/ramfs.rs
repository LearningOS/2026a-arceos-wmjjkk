use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::{string::String, vec::Vec};

use axfs_vfs::{
    impl_vfs_dir_default, impl_vfs_non_dir_default, VfsDirEntry, VfsError, VfsNodeAttr,
    VfsNodeOps, VfsNodeRef, VfsNodeType, VfsOps, VfsResult,
};
use axsync::Mutex;
use lazyinit::LazyInit;

/// A RAM filesystem that implements [`axfs_vfs::VfsOps`].
pub struct RamFileSystem {
    parent: LazyInit<VfsNodeRef>,
    root: Arc<DirNode>,
}

impl RamFileSystem {
    /// Create a new instance.
    pub fn new() -> Self {
        Self {
            parent: LazyInit::new(),
            root: DirNode::new(None),
        }
    }

    /// Returns the root directory node in [`Arc<DirNode>`](DirNode).
    #[allow(dead_code)]
    pub fn root_dir_node(&self) -> Arc<DirNode> {
        self.root.clone()
    }
}

impl VfsOps for RamFileSystem {
    fn mount(&self, _path: &str, mount_point: VfsNodeRef) -> VfsResult {
        if let Some(parent) = mount_point.parent() {
            if !self.parent.is_inited() {
                self.parent.init_once(parent);
            }
            self.root.set_parent(Some(&self.parent));
        } else {
            self.root.set_parent(None);
        }
        Ok(())
    }

    fn root_dir(&self) -> VfsNodeRef {
        self.root.clone()
    }
}

impl Default for RamFileSystem {
    fn default() -> Self {
        Self::new()
    }
}

/// The file node in the RAM filesystem.
pub struct FileNode {
    content: Mutex<Vec<u8>>,
}

impl FileNode {
    const fn new() -> Self {
        Self {
            content: Mutex::new(Vec::new()),
        }
    }
}

impl VfsNodeOps for FileNode {
    fn get_attr(&self) -> VfsResult<VfsNodeAttr> {
        Ok(VfsNodeAttr::new_file(self.content.lock().len() as _, 0))
    }

    fn truncate(&self, size: u64) -> VfsResult {
        let mut content = self.content.lock();
        if size < content.len() as u64 {
            content.truncate(size as _);
        } else {
            content.resize(size as _, 0);
        }
        Ok(())
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> VfsResult<usize> {
        let content = self.content.lock();
        let start = content.len().min(offset as usize);
        let end = content.len().min(offset as usize + buf.len());
        let src = &content[start..end];
        buf[..src.len()].copy_from_slice(src);
        Ok(src.len())
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> VfsResult<usize> {
        let offset = offset as usize;
        let mut content = self.content.lock();
        if offset + buf.len() > content.len() {
            content.resize(offset + buf.len(), 0);
        }
        let dst = &mut content[offset..offset + buf.len()];
        dst.copy_from_slice(&buf[..dst.len()]);
        Ok(buf.len())
    }

    impl_vfs_non_dir_default! {}
}

/// The directory node in the RAM filesystem.
pub struct DirNode {
    this: Weak<DirNode>,
    parent: Mutex<Weak<dyn VfsNodeOps>>,
    children: Mutex<BTreeMap<String, VfsNodeRef>>,
}

impl DirNode {
    fn new(parent: Option<Weak<dyn VfsNodeOps>>) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            this: this.clone(),
            parent: Mutex::new(parent.unwrap_or_else(|| Weak::<Self>::new())),
            children: Mutex::new(BTreeMap::new()),
        })
    }

    fn set_parent(&self, parent: Option<&VfsNodeRef>) {
        *self.parent.lock() = parent.map_or(Weak::<Self>::new() as _, Arc::downgrade);
    }

    fn exist(&self, name: &str) -> bool {
        self.children.lock().contains_key(name)
    }

    fn create_node(&self, name: &str, ty: VfsNodeType) -> VfsResult {
        if self.exist(name) {
            log::error!("AlreadyExists {}", name);
            return Err(VfsError::AlreadyExists);
        }
        let node: VfsNodeRef = match ty {
            VfsNodeType::File => Arc::new(FileNode::new()),
            VfsNodeType::Dir => Self::new(Some(self.this.clone())),
            _ => return Err(VfsError::Unsupported),
        };
        self.children.lock().insert(name.into(), node);
        Ok(())
    }

    fn remove_node(&self, name: &str) -> VfsResult {
        let mut children = self.children.lock();
        let node = children.get(name).ok_or(VfsError::NotFound)?;
        if let Some(dir) = node.as_any().downcast_ref::<DirNode>() {
            if !dir.children.lock().is_empty() {
                return Err(VfsError::DirectoryNotEmpty);
            }
        }
        children.remove(name);
        Ok(())
    }
}

impl VfsNodeOps for DirNode {
    fn get_attr(&self) -> VfsResult<VfsNodeAttr> {
        Ok(VfsNodeAttr::new_dir(4096, 0))
    }

    fn parent(&self) -> Option<VfsNodeRef> {
        self.parent.lock().upgrade()
    }

    fn lookup(self: Arc<Self>, path: &str) -> VfsResult<VfsNodeRef> {
        let (name, rest) = split_path(path);
        let node = match name {
            "" | "." => Ok(self.clone() as VfsNodeRef),
            ".." => self.parent().ok_or(VfsError::NotFound),
            _ => self
                .children
                .lock()
                .get(name)
                .cloned()
                .ok_or(VfsError::NotFound),
        }?;

        if let Some(rest) = rest {
            node.lookup(rest)
        } else {
            Ok(node)
        }
    }

    fn read_dir(&self, start_idx: usize, dirents: &mut [VfsDirEntry]) -> VfsResult<usize> {
        let children = self.children.lock();
        let mut children = children.iter().skip(start_idx.max(2) - 2);
        for (i, ent) in dirents.iter_mut().enumerate() {
            match i + start_idx {
                0 => *ent = VfsDirEntry::new(".", VfsNodeType::Dir),
                1 => *ent = VfsDirEntry::new("..", VfsNodeType::Dir),
                _ => {
                    if let Some((name, node)) = children.next() {
                        *ent = VfsDirEntry::new(name, node.get_attr().unwrap().file_type());
                    } else {
                        return Ok(i);
                    }
                }
            }
        }
        Ok(dirents.len())
    }

    fn create(&self, path: &str, ty: VfsNodeType) -> VfsResult {
        log::debug!("create {:?} at ramfs: {}", ty, path);
        let (name, rest) = split_path(path);
        if let Some(rest) = rest {
            match name {
                "" | "." => self.create(rest, ty),
                ".." => self.parent().ok_or(VfsError::NotFound)?.create(rest, ty),
                _ => {
                    let subdir = self
                        .children
                        .lock()
                        .get(name)
                        .ok_or(VfsError::NotFound)?
                        .clone();
                    subdir.create(rest, ty)
                }
            }
        } else if name.is_empty() || name == "." || name == ".." {
            Ok(()) // already exists
        } else {
            self.create_node(name, ty)
        }
    }

    fn remove(&self, path: &str) -> VfsResult {
        log::debug!("remove at ramfs: {}", path);
        let (name, rest) = split_path(path);
        if let Some(rest) = rest {
            match name {
                "" | "." => self.remove(rest),
                ".." => self.parent().ok_or(VfsError::NotFound)?.remove(rest),
                _ => {
                    let subdir = self
                        .children
                        .lock()
                        .get(name)
                        .ok_or(VfsError::NotFound)?
                        .clone();
                    subdir.remove(rest)
                }
            }
        } else if name.is_empty() || name == "." || name == ".." {
            Err(VfsError::InvalidInput) // remove '.' or '..'
        } else {
            self.remove_node(name)
        }
    }

    fn rename(&self, src_path: &str, dst_path: &str) -> VfsResult {
        log::debug!("rename at ramfs: {} -> {}", src_path, dst_path);
        let (src_name, src_rest) = split_path(src_path);
        let (dst_name, dst_rest) = split_path(dst_path);
        match (src_rest, dst_rest) {
            (Some(sr), Some(dr)) if src_name == dst_name => match src_name {
                "" | "." => self.rename(sr, dr),
                ".." => self.parent().ok_or(VfsError::NotFound)?.rename(sr, dr),
                _ => {
                    let subdir = self
                        .children
                        .lock()
                        .get(src_name)
                        .ok_or(VfsError::NotFound)?
                        .clone();
                    subdir.rename(sr, dr)
                }
            },
            (None, None) => {
                if src_name.is_empty()
                    || src_name == "."
                    || src_name == ".."
                    || dst_name.is_empty()
                    || dst_name == "."
                    || dst_name == ".."
                {
                    return Err(VfsError::InvalidInput);
                }
                let mut children = self.children.lock();
                if src_name == dst_name {
                    return if children.contains_key(src_name) {
                        Ok(())
                    } else {
                        Err(VfsError::NotFound)
                    };
                }
                let node = children.remove(src_name).ok_or(VfsError::NotFound)?;
                children.insert(dst_name.into(), node);
                Ok(())
            }
            _ => Err(VfsError::Unsupported),
        }
    }

    impl_vfs_dir_default! {}
}

fn split_path(path: &str) -> (&str, Option<&str>) {
    let trimmed_path = path.trim_start_matches('/');
    trimmed_path.find('/').map_or((trimmed_path, None), |n| {
        (&trimmed_path[..n], Some(&trimmed_path[n + 1..]))
    })
}
