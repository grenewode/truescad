use xplicit_primitive::{BoundingBox, NEG_INFINITY_BOX, Object, normal_from_object};
use bitset::BitSet;
use vertex_index::{Index, VarIndex, VertexIndex, neg_offset, offset};
use qef;
use {Mesh, Plane};
use cell_configs::CELL_CONFIGS;
use xplicit_types::{Float, Point, Vector};
use std::collections::{BTreeSet, HashMap};
use std::cell::{Cell, RefCell};
use std::{error, fmt};
use std::cmp;
use cgmath::Array;
use rand;

// How accurately find zero crossings.
const PRECISION: Float = 0.05;
// The acceptable error threshold when simplifying the mesh.
const RELATIVE_ERROR: Float = 2.0;

//  Edge indexes
//
//      +-------9-------+
//     /|              /|
//    7 |            10 |              ^
//   /  8            /  11            /
//  +-------6-------+   |     ^    higher indexes in y
//  |   |           |   |     |     /
//  |   +-------3---|---+     |    /
//  2  /            5  /  higher indexes
//  | 1             | 4      in z
//  |/              |/        |/
//  o-------0-------+         +-- higher indexes in x ---->
//
// Point o is the reference point of the current cell.
// All edges go from lower indexes to higher indexes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Edge {
    A = 0,
    B = 1,
    C = 2,
    D = 3,
    E = 4,
    F = 5,
    G = 6,
    H = 7,
    I = 8,
    J = 9,
    K = 10,
    L = 11,
}

impl Edge {
    pub fn from_usize(e: usize) -> Edge {
        match e {
            0 => Edge::A,
            1 => Edge::B,
            2 => Edge::C,
            3 => Edge::D,
            4 => Edge::E,
            5 => Edge::F,
            6 => Edge::G,
            7 => Edge::H,
            8 => Edge::I,
            9 => Edge::J,
            10 => Edge::K,
            11 => Edge::L,
            _ => panic!("Not edge for {:?}", e),
        }
    }
    pub fn base(&self) -> Edge {
        Edge::from_usize(*self as usize % 3)
    }
}

// Cell offsets of edges
const EDGE_OFFSET: [Index; 12] = [[0, 0, 0], [0, 0, 0], [0, 0, 0], [0, 1, 0], [1, 0, 0],
                                  [1, 0, 0], [0, 0, 1], [0, 0, 1], [0, 1, 0], [0, 1, 1],
                                  [1, 0, 1], [1, 1, 0]];

// Quad definition for edges 0-2.
const QUADS: [[Edge; 4]; 3] = [[Edge::A, Edge::G, Edge::J, Edge::D],
                               [Edge::B, Edge::E, Edge::K, Edge::H],
                               [Edge::C, Edge::I, Edge::L, Edge::F]];

#[derive(Debug)]
enum DualContouringError {
    HitZero(Point),
}

impl error::Error for DualContouringError {
    fn description(&self) -> &str {
        match self {
            &DualContouringError::HitZero(_) => "Hit zero value during grid sampling.",
        }
    }
}

impl fmt::Display for DualContouringError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            &DualContouringError::HitZero(p) => write!(f, "Hit zero value for {:?}", p),
        }
    }
}

// A vertex of the mesh. This can be either a primary vertex of the sampled mesh or a vertex
// generated by joining multiple vertices in the octree.
#[derive(Debug)]
struct Vertex {
    index: Index,
    qef: RefCell<qef::Qef>,
    neighbors: [Vec<VarIndex>; 6],
    parent: Cell<Option<usize>>,
    children: Vec<usize>,
    // Index of this vertex in the final mesh.
    mesh_index: Cell<Option<usize>>,
}


#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct EdgeIndex {
    edge: Edge,
    index: Index,
}

impl EdgeIndex {
    pub fn base(&self) -> EdgeIndex {
        EdgeIndex {
            edge: self.edge.base(),
            index: offset(self.index, EDGE_OFFSET[self.edge as usize]),
        }
    }
}

pub struct ManifoldDualContouring {
    object: Box<Object>,
    origin: Point,
    dim: [usize; 3],
    mesh: RefCell<Mesh>,
    res: Float,
    value_grid: HashMap<Index, Float>,
    edge_grid: RefCell<HashMap<EdgeIndex, Plane>>,
    // The Vertex Octtree. vertex_octtree[0] stores the leaf vertices. vertex_octtree[1] the next
    // layer and so on. vertex_octtree.len() is the depth of the octtree.
    vertex_octtree: Vec<Vec<Vertex>>,
    // Map from VertexIndex to vertex_octtree[0]
    vertex_index_map: HashMap<VertexIndex, usize>,
}

// Returns the next largest power of 2
fn pow2roundup(x: usize) -> usize {
    let mut x = x;
    x -= 1;
    x |= x >> 1;
    x |= x >> 2;
    x |= x >> 4;
    x |= x >> 8;
    x |= x >> 16;
    x |= x >> 32;
    return x + 1;
}


// Returns a BitSet containing all egdes connected to "edge" in this cell.
fn get_connected_edges(edge: Edge, cell: BitSet) -> BitSet {
    for &edge_set in CELL_CONFIGS[cell.as_u32() as usize].iter() {
        if edge_set.get(edge as usize) {
            return edge_set;
        }
    }
    panic!("Did not find edge_set for {:?} and {:?}", edge, cell);
}

// Returns all BitSets containing  egdes connected to one of edge_set in this cell.
fn get_connected_edges_from_edge_set(edge_set: BitSet, cell: BitSet) -> Vec<BitSet> {
    let mut result = Vec::new();
    for &cell_edge_set in CELL_CONFIGS[cell.as_u32() as usize].iter() {
        if !cell_edge_set.intersect(edge_set).empty() {
            result.push(cell_edge_set);
        }
    }
    debug_assert!(result.iter()
                        .fold(BitSet::zero(), |sum, x| sum.merge(*x))
                        .intersect(edge_set) == edge_set,
                  "result: {:?} does not contain all edges from egde_set: {:?}",
                  result,
                  edge_set);
    result
}

fn half_index(input: &Index) -> Index {
    [input[0] / 2, input[1] / 2, input[2] / 2]
}

// Will add the following vertices to neighbors:
// All vertices in the same octtree subcell as start and connected to start.
fn add_connected_vertices_in_subcell(base: &Vec<Vertex>,
                                     start: &Vertex,
                                     neigbors: &mut BTreeSet<usize>) {
    let parent_index = half_index(&start.index);
    for neighbor_index_vector in start.neighbors.iter() {
        for neighbor_index in neighbor_index_vector.iter() {
            match neighbor_index {
                &VarIndex::Index(vi) => {
                    let ref neighbor = base[vi];
                    if half_index(&neighbor.index) == parent_index {
                        if neigbors.insert(vi) {
                            add_connected_vertices_in_subcell(base, &base[vi], neigbors);
                        }
                    }
                }
                &VarIndex::VertexIndex(vi) => {
                    panic!("unexpected VertexIndex {:?}", vi);
                }
            }
        }
    }
}

fn add_child_to_parent(child: &Vertex, parent: &mut Vertex) {
    parent.qef.borrow_mut().merge(&*child.qef.borrow());
    for dim in 0..3 {
        let relevant_neighbor = dim * 2 + (child.index[dim] & 1);
        for neighbor in child.neighbors[relevant_neighbor].iter() {
            if !parent.neighbors[relevant_neighbor].contains(neighbor) {
                parent.neighbors[relevant_neighbor].push(*neighbor);
            }
        }
    }
}

fn subsample_octtree(base: &Vec<Vertex>) -> Vec<Vertex> {
    let mut result = Vec::new();
    for (i, vertex) in base.iter().enumerate() {
        if vertex.parent.get() == None {
            let mut neighbor_set = BTreeSet::new();
            neighbor_set.insert(i);
            add_connected_vertices_in_subcell(base, vertex, &mut neighbor_set);
            let mut parent = Vertex {
                index: half_index(&vertex.index),
                qef: RefCell::new(qef::Qef::new(&[], NEG_INFINITY_BOX.clone())),
                neighbors: [Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()],
                parent: Cell::new(None),
                children: Vec::new(),
                mesh_index: Cell::new(None),
            };
            for &neighbor_index in neighbor_set.iter() {
                let child = &base[neighbor_index];
                debug_assert!(child.parent.get() == None,
                              "child #{:?} already has parent #{:?}",
                              neighbor_index,
                              child.parent.get().unwrap());
                debug_assert!(!parent.children.contains(&neighbor_index));
                parent.children.push(neighbor_index);
                add_child_to_parent(child, &mut parent);
                child.parent.set(Some(result.len()));
            }
            result.push(parent);
        }
    }
    for vertex in result.iter_mut() {
        for neighbor_vec in vertex.neighbors.iter_mut() {
            for neighbor in neighbor_vec.iter_mut() {
                match neighbor {
                    &mut VarIndex::VertexIndex(_) => panic!("unexpected VertexIndex in normal node."),
                    &mut VarIndex::Index(i) => {
                        *neighbor = VarIndex::Index(base[i].parent.get().unwrap())
                    }
                }
            }
        }
    }
    result
}

struct Timer {
    t: ::time::Tm,
}

impl Timer {
    fn new() -> Timer {
        Timer { t: ::time::now() }
    }
    fn elapsed(&mut self) -> ::time::Duration {
        let now = ::time::now();
        let result = now - self.t;
        self.t = now;
        result
    }
}

impl ManifoldDualContouring {
    // Constructor
    // obj: Object to tessellate
    // res: resolution
    pub fn new(obj: Box<Object>, res: Float) -> ManifoldDualContouring {
        let bbox = obj.bbox().dilate(1. + res * 1.1);
        println!("ManifoldDualContouring: res: {:} {:?}", res, bbox);
        ManifoldDualContouring {
            object: obj,
            origin: bbox.min,
            dim: [(bbox.dim()[0] / res).ceil() as usize,
                  (bbox.dim()[1] / res).ceil() as usize,
                  (bbox.dim()[2] / res).ceil() as usize],
            mesh: RefCell::new(Mesh {
                vertices: Vec::new(),
                faces: Vec::new(),
            }),
            res: res,
            value_grid: HashMap::new(),
            edge_grid: RefCell::new(HashMap::new()),
            vertex_octtree: Vec::new(),
            vertex_index_map: HashMap::new(),
        }
    }
    pub fn tessellate(&mut self) -> Mesh {
        loop {
            match self.try_tessellate() {
                Ok(mesh) => return mesh,
                Err(x) => {
                    let padding = self.res / (10. + rand::random::<Float>().abs());
                    println!("Error: {:?}. moving by {:?} and retrying.", x, padding);
                    self.origin.x -= padding;
                    self.value_grid.clear();
                    self.mesh.borrow_mut().vertices.clear();
                    self.mesh.borrow_mut().faces.clear();
                    self.vertex_octtree.clear();
                    self.vertex_index_map.clear();
                }
            }
        }
    }

    fn sample_value_grid(&mut self,
                         idx: Index,
                         pos: Point,
                         size: usize,
                         val: Float)
                         -> Option<DualContouringError> {
        debug_assert!(size > 1);
        let mut midx = idx;
        let size = size / 2;
        let vpos = [pos, pos + size as Float * Vector::new(self.res, self.res, self.res)];
        let sub_cube_diagonal = size as Float * self.res * (3. as Float).sqrt();

        for z in 0..2 {
            for y in 0..2 {
                for x in 0..2 {
                    let mpos = Point::new(vpos[x].x, vpos[y].y, vpos[z].z);
                    let value = if midx == idx {
                        val
                    } else {
                        self.object.approx_value(mpos, self.res)
                    };

                    if value == 0. {
                        return Some(DualContouringError::HitZero(mpos));
                    }

                    if size > 1 && value.abs() <= sub_cube_diagonal {
                        if let Some(e) = self.sample_value_grid(midx, mpos, size, value) {
                            return Some(e);
                        }
                    } else {
                        self.value_grid.insert(midx, value);
                    }
                    midx[0] += size;
                }
                midx[0] -= 2 * size;
                midx[1] += size;
            }
            midx[1] -= 2 * size;
            midx[2] += size;
        }
        None
    }

    // This method does the main work of tessellation.
    fn try_tessellate(&mut self) -> Result<Mesh, DualContouringError> {
        let res = self.res;
        let mut t = Timer::new();

        let maxdim = cmp::max(self.dim[0], cmp::max(self.dim[1], self.dim[2]));
        let origin = self.origin;
        let origin_value = self.object.approx_value(origin, res);

        if let Some(e) = self.sample_value_grid([0, 0, 0],
                                                origin,
                                                pow2roundup(maxdim),
                                                origin_value) {
            return Err(e);
        }

        println!("generated value_grid: {:}", t.elapsed());
        println!("value_grid with {:} for {:} cells.",
                 self.value_grid.len(),
                 self.dim[0] * self.dim[1] * self.dim[2]);

        // Store crossing positions of edges in edge_grid
        {
            let mut edge_grid = self.edge_grid.borrow_mut();
            for (&point_idx, &point_value) in &self.value_grid {
                for &edge in [Edge::A, Edge::B, Edge::C].iter() {
                    let mut adjacent_idx = point_idx.clone();
                    adjacent_idx[edge as usize] += 1;
                    if let Some(&adjacent_value) = self.value_grid
                                                       .get(&adjacent_idx) {
                        let point_pos = self.origin +
                                        res *
                                        Vector::new(point_idx[0] as Float,
                                                    point_idx[1] as Float,
                                                    point_idx[2] as Float);
                        let mut adjacent_pos = point_pos;
                        adjacent_pos[edge as usize] += res;
                        if let Some(plane) = self.find_zero(point_pos,
                                                            point_value,
                                                            adjacent_pos,
                                                            adjacent_value) {
                            edge_grid.insert(EdgeIndex {
                                                 edge: edge,
                                                 index: point_idx,
                                             },
                                             plane);
                        }
                    }
                }
            }
        }
        println!("generated edge_grid: {:}", t.elapsed());

        let (leafs, index_map) = self.generate_leaf_vertices();
        self.vertex_index_map = index_map;
        self.vertex_octtree.push(leafs);

        println!("generated {:?} leaf vertices: {:}",
                 self.vertex_octtree[0].len(),
                 t.elapsed());

        loop {
            let next = subsample_octtree(self.vertex_octtree.last().unwrap());
            if next.len() == self.vertex_octtree.last().unwrap().len() {
                break;
            }
            println!("layer #{} {} vertices {:}",
                     self.vertex_octtree.len(),
                     next.len(),
                     t.elapsed());
            self.vertex_octtree.push(next);
        }

        let num_qefs_solved = self.solve_qefs();

        println!("solved {} qefs: {:}", num_qefs_solved, t.elapsed());

        for edge_index in self.edge_grid.borrow().keys() {
            self.compute_quad(*edge_index);
        }
        println!("generated quads: {:}", t.elapsed());

        println!("computed mesh with {:?} faces.",
                 self.mesh.borrow().faces.len());

        Ok(self.mesh.borrow().clone())
    }


    // Solves QEFs in vertex stack, starting at the highest level, down all layers until the qef error
    // is below threshold.
    // Returns the number of solved QEFs.
    fn solve_qefs(&self) -> usize {
        let mut num_solved = 0;
        if let Some(top_layer) = self.vertex_octtree.last() {
            for i in 0..top_layer.len() {
                num_solved += self.recursively_solve_qefs(&self.vertex_octtree.len() - 1, i);
            }
        }
        num_solved
    }

    fn recursively_solve_qefs(&self, layer: usize, index_in_layer: usize) -> usize {
        let vertex = &self.vertex_octtree[layer][index_in_layer];
        assert!(vertex.children.len() == 0 || layer > 0);
        let error;
        {
            // Solve qef and store error.
            let mut qef = vertex.qef.borrow_mut();
            // Make sure we never solve a qef twice.
            debug_assert!(qef.error.is_nan(),
                          "found solved qef layer {:?} index {:?} {:?} parent: {:?}",
                          layer,
                          index_in_layer,
                          vertex.index,
                          vertex.parent);
            qef.solve();
            error = qef.error;
        }
        let mut num_solved = 1;
        // If error exceed threshold, recurse into subvertices.
        if error.abs() > self.res * RELATIVE_ERROR {
            for &child_index in vertex.children.iter() {
                num_solved += self.recursively_solve_qefs(layer - 1, child_index);
            }
        }
        num_solved
    }

    // Generates leaf vertices along with a map that points VertexIndices to the index in the leaf
    // vertex vec.
    fn generate_leaf_vertices(&self) -> (Vec<Vertex>, HashMap<VertexIndex, usize>) {
        let mut index_map = HashMap::new();
        let mut vertices = Vec::new();
        for edge_index in self.edge_grid.borrow().keys() {
            self.add_vertices_for_minimal_egde(edge_index, &mut vertices, &mut index_map);
        }
        for vertex in vertices.iter_mut() {
            for neighbor_vec in vertex.neighbors.iter_mut() {
                for neighbor in neighbor_vec.iter_mut() {
                    match neighbor {
                        &mut VarIndex::VertexIndex(vi) => {
                            *neighbor = VarIndex::Index(*index_map.get(&vi).unwrap())
                        }
                        &mut VarIndex::Index(_) => panic!("unexpected Index in fresh leaf map."),
                    }
                }
            }
        }
        for vi in 0..vertices.len() {
            for np in 0..vertices[vi].neighbors.len() {
                for ni in 0..vertices[vi].neighbors[np].len() {
                    match vertices[vi].neighbors[np][ni] {
                        VarIndex::VertexIndex(_) => panic!("unexpected VertexIndex."),
                        VarIndex::Index(i) => {
                            debug_assert!(vertices[i].neighbors[np ^ 1]
                                              .contains(&VarIndex::Index(vi)),
                                          "vertex[{}].neighbors[{}][{}]=={:?}, but vertex[{}].neighbors[{}]=={:?}\n{:?} vs. {:?}",
                                          vi,
                                          np,
                                          ni,
                                          vertices[vi].neighbors[np][ni],
                                          i,
                                          np ^ 1,
                                          vertices[i].neighbors[np ^ 1],
                                          vertices[vi],
                                          vertices[i]);
                        }
                    }
                }
            }
        }
        (vertices, index_map)
    }
    fn add_vertices_for_minimal_egde(&self,
                                     edge_index: &EdgeIndex,
                                     vertices: &mut Vec<Vertex>,
                                     index_map: &mut HashMap<VertexIndex, usize>) {
        debug_assert!((edge_index.edge as usize) < 4);
        let cell_size = Vector::new(self.res, self.res, self.res);
        for &quad_egde in QUADS[edge_index.edge as usize].iter() {
            let idx = neg_offset(edge_index.index, EDGE_OFFSET[quad_egde as usize]);

            let edge_set = get_connected_edges(quad_egde, self.bitset_for_cell(idx));
            let vertex_index = VertexIndex {
                edges: edge_set,
                index: idx,
            };
            index_map.entry(vertex_index).or_insert_with(|| {
                let mut neighbors = [Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(),
                                     Vec::new()];
                for i in 0..6 {
                    if let Some(mut neighbor_index) = vertex_index.neighbor(i) {
                        for edges in get_connected_edges_from_edge_set(neighbor_index.edges,
                                                 self.bitset_for_cell(neighbor_index.index)) {
                            neighbor_index.edges = edges;
                            let idx = VarIndex::VertexIndex(neighbor_index);
                            if !neighbors[i].contains(&idx) {
                                neighbors[i].push(idx);
                            }
                        }
                    }
                }
                let tangent_planes: Vec<_> = edge_set.into_iter()
                                                     .map(|edge| {
                                                         self.get_edge_tangent_plane(&EdgeIndex {
                                                             edge: Edge::from_usize(edge),
                                                             index: idx,
                                                         })
                                                     })
                                                     .collect();
                let cell_origin = self.origin +
                                  Vector::new(idx[0] as Float, idx[1] as Float, idx[2] as Float) *
                                  self.res;
                vertices.push(Vertex {
                    index: idx,
                    qef: RefCell::new(qef::Qef::new(&tangent_planes,
                                                    BoundingBox::new(cell_origin,
                                                                     cell_origin + cell_size))),
                    neighbors: neighbors,
                    parent: Cell::new(None),
                    children: Vec::new(),
                    mesh_index: Cell::new(None),
                });
                vertices.len() - 1
            });
        }
    }

    fn get_edge_tangent_plane(&self, edge_index: &EdgeIndex) -> Plane {
        if let Some(ref plane) = self.edge_grid
                                     .borrow()
                                     .get(&edge_index.base()) {
            return *plane.clone();
        }
        panic!("could not find edge_point: {:?} -> {:?}",
               edge_index,
               edge_index.base());
    }

    // Return the Point index (in self.mesh.vertices) the the point belonging to edge/idx.
    fn lookup_cell_point(&self, edge: Edge, idx: Index) -> usize {
        // Generate the proper vertex Index from a single edge and an Index.
        let edge_set = get_connected_edges(edge, self.bitset_for_cell(idx));
        let vertex_index = VertexIndex {
            edges: edge_set,
            index: idx,
        };

        // Convert the vertex index to index and layer in the Octtree.
        let mut octtree_index = *self.vertex_index_map.get(&vertex_index).unwrap();
        let mut octtree_layer = 0;
        // Walk up the chain of parents
        loop {
            let next_index = self.vertex_octtree[octtree_layer][octtree_index]
                                 .parent
                                 .get()
                                 .unwrap();
            let error = self.vertex_octtree[octtree_layer + 1][next_index].qef.borrow().error;
            if (!error.is_nan() && error > (self.res * RELATIVE_ERROR)) ||
               (octtree_layer == self.vertex_octtree.len() - 2) {
                // Stop, if either the error is too large or we will reach the top.
                break;
            }
            octtree_layer += 1;
            octtree_index = next_index;
        }
        let vertex = &self.vertex_octtree[octtree_layer][octtree_index];
        // If the vertex exists in mesh, return its index.
        if let Some(mesh_index) = vertex.mesh_index.get() {
            return mesh_index;
        }
        // If not, store it in mesh and return its index.
        let qef_solution = vertex.qef.borrow().solution;
        let ref mut vertex_list = self.mesh.borrow_mut().vertices;
        let result = vertex_list.len();
        vertex.mesh_index.set(Some(result));
        vertex_list.push([qef_solution.x, qef_solution.y, qef_solution.z]);
        return result;
    }

    fn bitset_for_cell(&self, idx: Index) -> BitSet {
        let mut idx = idx;
        let mut result = BitSet::zero();
        for z in 0..2 {
            for y in 0..2 {
                for x in 0..2 {
                    if let Some(&v) = self.value_grid.get(&idx) {
                        if v < 0. {
                            result.set(z << 2 | y << 1 | x);
                        }
                    } else {
                        panic!("did not find value_grid[{:?}]", idx);
                    }
                    idx[0] += 1;
                }
                idx[0] -= 2;
                idx[1] += 1;
            }
            idx[1] -= 2;
            idx[2] += 1;
        }
        result
    }

    // Compute a quad for the given edge and append it to the list.
    fn compute_quad(&self, edge_index: EdgeIndex) {
        debug_assert!((edge_index.edge as usize) < 4);
        debug_assert!(edge_index.index.iter().all(|&i| i > 0));

        let mut p = Vec::with_capacity(4);
        for &quad_egde in QUADS[edge_index.edge as usize].iter() {
            let point_index = self.lookup_cell_point(quad_egde,
                                                     neg_offset(edge_index.index,
                                                                EDGE_OFFSET[quad_egde as usize]));
            // Dedup points before insertion (two minimal vertices might end up in the same parent
            // vertex).
            if !p.contains(&point_index) {
                p.push(point_index)
            }
        }
        // Only try to generate meshes, if there are more then two points.
        if p.len() < 3 {
            return;
        }
        // Reverse order, if the edge is reversed.
        if let Some(&v) = self.value_grid.get(&edge_index.index) {
            if v < 0. {
                p.reverse();
            }
        }
        let ref mut face_list = self.mesh.borrow_mut().faces;
        // TODO: Fix this to choose the proper split.
        face_list.push([p[0], p[1], p[2]]);
        if p.len() == 4 {
            face_list.push([p[2], p[3], p[0]]);
        }
    }

    // If a is inside the object and b outside - this method return the point on the line between
    // a and b where the object edge is. It also returns the normal on that point.
    // av and bv represent the object values at a and b.
    fn find_zero(&self, a: Point, av: Float, b: Point, bv: Float) -> Option<(Plane)> {
        debug_assert!(av == self.object.approx_value(a, self.res));
        debug_assert!(bv == self.object.approx_value(b, self.res));
        assert!(a != b);
        if av.signum() == bv.signum() {
            return None;
        }
        let mut distance = (a - b).min().abs().max((a - b).max());
        distance = distance.min(av.abs()).min(bv.abs());
        if distance < PRECISION * self.res {
            let mut result = &a;
            if bv.abs() < av.abs() {
                result = &b;
            }
            return Some(Plane {
                p: *result,
                // We need a precise normal here.
                n: normal_from_object(&*self.object, *result),
            });
        }
        // Linear interpolation of the zero crossing.
        let n = a + (b - a) * (av.abs() / (bv - av).abs());
        let nv = self.object.approx_value(n, self.res);

        if av.signum() != nv.signum() {
            return self.find_zero(a, av, n, nv);
        } else {
            return self.find_zero(n, nv, b, bv);
        }
    }
}


#[cfg(test)]
mod tests {
    use super::get_connected_edges_from_edge_set;
    use super::super::bitset::BitSet;
    //  Corner indexes
    //
    //      6---------------7
    //     /|              /|
    //    / |             / |
    //   /  |            /  |
    //  4---------------5   |
    //  |   |           |   |
    //  |   2-----------|---3
    //  |  /            |  /
    //  | /             | /
    //  |/              |/
    //  0---------------1

    //  Edge indexes
    //
    //      +-------9-------+
    //     /|              /|
    //    7 |            10 |              ^
    //   /  8            /  11            /
    //  +-------6-------+   |     ^    higher indexes in y
    //  |   |           |   |     |     /
    //  |   +-------3---|---+     |    /
    //  2  /            5  /  higher indexes
    //  | 1             | 4      in z
    //  |/              |/        |/
    //  o-------0-------+         +-- higher indexes in x ---->

    #[test]
    fn connected_edges() {
        let cell = BitSet::from_4bits(0, 6, 3, 5);
        let edge_set = BitSet::from_4bits(4, 5, 10, 11);
        let connected_edges = get_connected_edges_from_edge_set(edge_set, cell);
        assert_eq!(connected_edges.len(), 2);
        assert!(connected_edges.contains(&BitSet::from_4bits(5, 5, 6, 10)));
        assert!(connected_edges.contains(&BitSet::from_4bits(3, 3, 4, 11)));
    }
}